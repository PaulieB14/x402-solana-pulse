//! x402 Solana Pulse - Substreams v1.0.0
//!
//! Real-time analytics for the Coinbase x402 payment protocol on Solana.
//!
//! Detects x402 settlements via the SVM `exact` scheme:
//! https://github.com/coinbase/x402/blob/main/specs/schemes/exact/scheme_exact_svm.md
//!
//! A valid x402 settlement on Solana has:
//!   - An SPL Token or Token-2022 `TransferChecked` instruction
//!   - An SPL Memo instruction (payment reference / nonce)
//!   - Transaction fee payer distinct from the TransferChecked authority
//!     (the facilitator sponsors, the payer signs)
//!
//! Module layers:
//! - Layer 1: Settlement extraction (map_x402_settlements)
//! - Layer 2: State stores (payer/recipient/facilitator volume, counts, fees)
//! - Layer 3: Analytics (map_payer_stats, map_recipient_stats, map_facilitator_stats)
//! - Layer 4: SQL sink (db_out)

mod pb;

use pb::x402::v1 as x402;
use substreams::prelude::*;
use substreams::scalar::BigInt;
use substreams::store::{
    StoreAddBigInt, StoreAddInt64, StoreGet, StoreGetBigInt, StoreGetInt64,
    StoreSetIfNotExistsInt64,
};
use substreams_database_change::pb::database::DatabaseChanges;
use substreams_database_change::tables::Tables;
use substreams_solana::pb::sf::solana::r#type::v1 as sol;

// =============================================
// Program IDs (base58)
// =============================================

/// SPL Token program
const SPL_TOKEN_PROGRAM: &str = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";

/// SPL Token-2022 program
const TOKEN_2022_PROGRAM: &str = "TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb";

/// SPL Memo v2 program (current)
const SPL_MEMO_V2: &str = "MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr";

/// SPL Memo v1 program (legacy, still valid)
const SPL_MEMO_V1: &str = "Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo";

/// SPL Token `TransferChecked` instruction discriminator
const TRANSFER_CHECKED: u8 = 12;

/// Convert Unix timestamp seconds to PostgreSQL TIMESTAMP format
fn unix_to_timestamp(secs: i64) -> String {
    let days_since_epoch = secs / 86400;
    let time_of_day = secs % 86400;
    let hours = time_of_day / 3600;
    let minutes = (time_of_day % 3600) / 60;
    let seconds = time_of_day % 60;

    let mut days = days_since_epoch;
    let mut year = 1970i64;
    loop {
        let diy = if is_leap_year(year) { 366 } else { 365 };
        if days < diy {
            break;
        }
        days -= diy;
        year += 1;
    }

    let dim: [i64; 12] = if is_leap_year(year) {
        [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    } else {
        [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31]
    };

    let mut month = 1;
    for &d in &dim {
        if days < d {
            break;
        }
        days -= d;
        month += 1;
    }
    let day = days + 1;

    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        year, month, day, hours, minutes, seconds
    )
}

fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || (y % 400 == 0)
}

/// Encode raw pubkey bytes as base58 string
fn b58(bytes: &[u8]) -> String {
    bs58::encode(bytes).into_string()
}

/// Build the resolved account keys for a transaction: static account keys
/// followed by loaded writable and readonly addresses from the meta.
fn resolved_keys(
    msg: &sol::Message,
    meta: &sol::TransactionStatusMeta,
) -> Vec<Vec<u8>> {
    let mut keys: Vec<Vec<u8>> = msg.account_keys.iter().cloned().collect();
    keys.extend(meta.loaded_writable_addresses.iter().cloned());
    keys.extend(meta.loaded_readonly_addresses.iter().cloned());
    keys
}

/// Decode a `TransferChecked` instruction's data section.
/// Returns (amount, decimals) on success.
/// Layout: [discriminator (1 byte = 12), amount (u64 LE), decimals (u8)]
fn decode_transfer_checked(data: &[u8]) -> Option<(u64, u8)> {
    if data.len() < 10 || data[0] != TRANSFER_CHECKED {
        return None;
    }
    let amount = u64::from_le_bytes(data[1..9].try_into().ok()?);
    let decimals = data[9];
    Some((amount, decimals))
}

/// Describes a candidate x402 settlement instruction found in a transaction.
struct TransferCheckedCall<'a> {
    instruction_index: u32,
    token_program: &'a str,
    source_idx: u8,
    mint_idx: u8,
    destination_idx: u8,
    authority_idx: u8,
    amount: u64,
    decimals: u8,
}

/// Scan a single instruction and return a TransferCheckedCall if it is an
/// SPL Token / Token-2022 TransferChecked. Accepts primitive fields so it
/// works for both top-level CompiledInstruction and nested InnerInstruction.
fn try_transfer_checked<'a>(
    accounts: &[u8],
    data: &[u8],
    ix_index: u32,
    program_id: &'a str,
) -> Option<TransferCheckedCall<'a>> {
    let token_program = match program_id {
        SPL_TOKEN_PROGRAM => SPL_TOKEN_PROGRAM,
        TOKEN_2022_PROGRAM => TOKEN_2022_PROGRAM,
        _ => return None,
    };
    if accounts.len() < 4 {
        return None;
    }
    let (amount, decimals) = decode_transfer_checked(data)?;
    Some(TransferCheckedCall {
        instruction_index: ix_index,
        token_program,
        source_idx: accounts[0],
        mint_idx: accounts[1],
        destination_idx: accounts[2],
        authority_idx: accounts[3],
        amount,
        decimals,
    })
}

/// Returns true if the program_id is an SPL Memo program (v1 or v2).
fn is_memo_program(program_id: &str) -> bool {
    program_id == SPL_MEMO_V2 || program_id == SPL_MEMO_V1
}

// =============================================
// LAYER 1: Settlement Extraction
// =============================================

/// Extract x402 payment settlements from Solana blocks.
#[substreams::handlers::map]
fn map_x402_settlements(
    blk: sol::Block,
) -> Result<x402::Settlements, substreams::errors::Error> {
    let block_timestamp = blk
        .block_time
        .as_ref()
        .map(|t| prost_types::Timestamp {
            seconds: t.timestamp,
            nanos: 0,
        });
    let slot = blk.slot;

    let mut out = x402::Settlements {
        slot,
        block_timestamp: block_timestamp.clone(),
        ..Default::default()
    };

    for confirmed_tx in blk.transactions() {
        let Some(meta) = confirmed_tx.meta.as_ref() else {
            continue;
        };
        let Some(tx) = confirmed_tx.transaction.as_ref() else {
            continue;
        };
        let Some(msg) = tx.message.as_ref() else {
            continue;
        };
        if tx.signatures.is_empty() || msg.account_keys.is_empty() {
            continue;
        }

        let keys = resolved_keys(msg, meta);

        // Fee payer is always the first signer (first account key).
        let fee_payer = b58(&keys[0]);
        let signature = b58(&tx.signatures[0]);
        let fee_lamports = meta.fee;

        // Walk all instructions (top-level + inner) and classify them.
        // We use compiled instructions directly so we have account indexes
        // for post_token_balances lookup.
        let mut transfer_checked_calls: Vec<TransferCheckedCall> = Vec::new();
        let mut memo_datas: Vec<Vec<u8>> = Vec::new();
        let mut running_ix_index: u32 = 0;

        for (top_idx, ix) in msg.instructions.iter().enumerate() {
            let prog_idx = ix.program_id_index as usize;
            if prog_idx >= keys.len() {
                continue;
            }
            let program_id = b58(&keys[prog_idx]);

            if let Some(tc) = try_transfer_checked(
                &ix.accounts,
                &ix.data,
                running_ix_index,
                program_id_to_static(&program_id),
            ) {
                transfer_checked_calls.push(tc);
            } else if is_memo_program(&program_id) {
                memo_datas.push(ix.data.clone());
            }
            running_ix_index += 1;

            // Inner instructions associated with this top-level index
            for inner_set in meta.inner_instructions.iter() {
                if inner_set.index as usize != top_idx {
                    continue;
                }
                for inner_ix in &inner_set.instructions {
                    let inner_prog_idx = inner_ix.program_id_index as usize;
                    if inner_prog_idx >= keys.len() {
                        continue;
                    }
                    let inner_program_id = b58(&keys[inner_prog_idx]);
                    if let Some(tc) = try_transfer_checked(
                        &inner_ix.accounts,
                        &inner_ix.data,
                        running_ix_index,
                        program_id_to_static(&inner_program_id),
                    ) {
                        transfer_checked_calls.push(tc);
                    } else if is_memo_program(&inner_program_id) {
                        memo_datas.push(inner_ix.data.clone());
                    }
                    running_ix_index += 1;
                }
            }
        }

        // x402 settlement must have at least one TransferChecked + one Memo
        if transfer_checked_calls.is_empty() || memo_datas.is_empty() {
            continue;
        }

        let memo = memo_datas
            .first()
            .map(|d| String::from_utf8_lossy(d).to_string())
            .unwrap_or_default();

        for tc in transfer_checked_calls {
            if (tc.authority_idx as usize) >= keys.len()
                || (tc.destination_idx as usize) >= keys.len()
                || (tc.mint_idx as usize) >= keys.len()
            {
                continue;
            }

            let payer = b58(&keys[tc.authority_idx as usize]);
            // Facilitator-sponsored heuristic: fee payer must differ from authority
            if payer == fee_payer {
                continue;
            }

            let destination_ata = b58(&keys[tc.destination_idx as usize]);
            let mint_from_instr = b58(&keys[tc.mint_idx as usize]);

            // Resolve recipient = owner of destination ATA via post_token_balances
            let recipient = meta
                .post_token_balances
                .iter()
                .find(|b| b.account_index == tc.destination_idx as u32)
                .map(|b| b.owner.clone())
                .unwrap_or_default();

            // Prefer mint from post_token_balances when present (authoritative)
            let mint = meta
                .post_token_balances
                .iter()
                .find(|b| b.account_index == tc.destination_idx as u32)
                .map(|b| b.mint.clone())
                .filter(|s| !s.is_empty())
                .unwrap_or(mint_from_instr);

            out.settlements.push(x402::Settlement {
                id: format!("{}-{}", signature, tc.instruction_index),
                signature: signature.clone(),
                instruction_index: tc.instruction_index,
                slot,
                timestamp: block_timestamp.clone(),
                payer,
                recipient,
                destination_ata,
                mint,
                amount: tc.amount.to_string(),
                decimals: tc.decimals as u32,
                token_program: tc.token_program.to_string(),
                facilitator: fee_payer.clone(),
                fee_lamports: fee_lamports.to_string(),
                memo: memo.clone(),
            });
        }
    }

    Ok(out)
}

/// Returns a 'static string slice when the program_id matches one of the
/// known token programs, else returns the input slice.
fn program_id_to_static(program_id: &str) -> &'static str {
    match program_id {
        SPL_TOKEN_PROGRAM => SPL_TOKEN_PROGRAM,
        TOKEN_2022_PROGRAM => TOKEN_2022_PROGRAM,
        SPL_MEMO_V2 => SPL_MEMO_V2,
        SPL_MEMO_V1 => SPL_MEMO_V1,
        _ => "",
    }
}

// =============================================
// LAYER 2: State Stores
// =============================================

#[substreams::handlers::store]
fn store_payer_volume(settlements: x402::Settlements, store: StoreAddBigInt) {
    for s in settlements.settlements {
        if s.payer.is_empty() {
            continue;
        }
        let amount = BigInt::try_from(&s.amount).unwrap_or_else(|_| BigInt::zero());
        store.add(0, &s.payer, &amount);
    }
}

#[substreams::handlers::store]
fn store_payer_count(settlements: x402::Settlements, store: StoreAddInt64) {
    for s in settlements.settlements {
        if s.payer.is_empty() {
            continue;
        }
        store.add(0, &s.payer, 1);
    }
}

#[substreams::handlers::store]
fn store_recipient_volume(settlements: x402::Settlements, store: StoreAddBigInt) {
    for s in settlements.settlements {
        if s.recipient.is_empty() {
            continue;
        }
        let amount = BigInt::try_from(&s.amount).unwrap_or_else(|_| BigInt::zero());
        store.add(0, &s.recipient, &amount);
    }
}

#[substreams::handlers::store]
fn store_recipient_count(settlements: x402::Settlements, store: StoreAddInt64) {
    for s in settlements.settlements {
        if s.recipient.is_empty() {
            continue;
        }
        store.add(0, &s.recipient, 1);
    }
}

#[substreams::handlers::store]
fn store_facilitator_volume(settlements: x402::Settlements, store: StoreAddBigInt) {
    for s in settlements.settlements {
        if s.facilitator.is_empty() {
            continue;
        }
        let amount = BigInt::try_from(&s.amount).unwrap_or_else(|_| BigInt::zero());
        store.add(0, &s.facilitator, &amount);
    }
}

#[substreams::handlers::store]
fn store_facilitator_count(settlements: x402::Settlements, store: StoreAddInt64) {
    for s in settlements.settlements {
        if s.facilitator.is_empty() {
            continue;
        }
        store.add(0, &s.facilitator, 1);
    }
}

#[substreams::handlers::store]
fn store_facilitator_fees(settlements: x402::Settlements, store: StoreAddBigInt) {
    // Tx fees counted once per tx (not per settlement).
    let mut seen: Vec<String> = Vec::new();
    for s in settlements.settlements {
        if s.facilitator.is_empty() || seen.contains(&s.signature) {
            continue;
        }
        seen.push(s.signature.clone());
        let fee = BigInt::try_from(&s.fee_lamports).unwrap_or_else(|_| BigInt::zero());
        store.add(0, &s.facilitator, &fee);
    }
}

#[substreams::handlers::store]
fn store_first_seen(settlements: x402::Settlements, store: StoreSetIfNotExistsInt64) {
    let ts = settlements
        .block_timestamp
        .as_ref()
        .map(|t| t.seconds)
        .unwrap_or(0);
    for s in settlements.settlements {
        if !s.payer.is_empty() {
            store.set_if_not_exists(0, format!("payer:{}", s.payer), &ts);
        }
        if !s.recipient.is_empty() {
            store.set_if_not_exists(0, format!("recipient:{}", s.recipient), &ts);
        }
        if !s.facilitator.is_empty() {
            store.set_if_not_exists(0, format!("facilitator:{}", s.facilitator), &ts);
        }
    }
}

// =============================================
// LAYER 3: Analytics
// =============================================

#[substreams::handlers::map]
fn map_payer_stats(
    settlements: x402::Settlements,
    volume_deltas: Deltas<DeltaBigInt>,
    count_store: StoreGetInt64,
    first_seen_store: StoreGetInt64,
) -> Result<x402::PayerStats, substreams::errors::Error> {
    let mut stats = x402::PayerStats {
        slot: settlements.slot,
        ..Default::default()
    };
    for delta in volume_deltas.deltas {
        let payer = delta.key.clone();
        let total_payments = count_store.get_last(&payer).unwrap_or(0) as u64;
        let first_payment_at = first_seen_store
            .get_last(format!("payer:{}", payer))
            .map(|secs| prost_types::Timestamp { seconds: secs, nanos: 0 });
        stats.stats.push(x402::PayerStat {
            payer_address: payer,
            total_spent: delta.new_value.to_string(),
            total_payments,
            first_payment_at,
            last_payment_at: settlements.block_timestamp.clone(),
        });
    }
    Ok(stats)
}

#[substreams::handlers::map]
fn map_recipient_stats(
    settlements: x402::Settlements,
    volume_deltas: Deltas<DeltaBigInt>,
    count_store: StoreGetInt64,
    first_seen_store: StoreGetInt64,
) -> Result<x402::RecipientStats, substreams::errors::Error> {
    let mut stats = x402::RecipientStats {
        slot: settlements.slot,
        ..Default::default()
    };
    for delta in volume_deltas.deltas {
        let recipient = delta.key.clone();
        let total_payments = count_store.get_last(&recipient).unwrap_or(0) as u64;
        let first_payment_at = first_seen_store
            .get_last(format!("recipient:{}", recipient))
            .map(|secs| prost_types::Timestamp { seconds: secs, nanos: 0 });
        stats.stats.push(x402::RecipientStat {
            recipient_address: recipient,
            total_received: delta.new_value.to_string(),
            total_payments,
            first_payment_at,
            last_payment_at: settlements.block_timestamp.clone(),
        });
    }
    Ok(stats)
}

#[substreams::handlers::map]
fn map_facilitator_stats(
    settlements: x402::Settlements,
    volume_deltas: Deltas<DeltaBigInt>,
    count_store: StoreGetInt64,
    fees_store: StoreGetBigInt,
    first_seen_store: StoreGetInt64,
) -> Result<x402::FacilitatorStats, substreams::errors::Error> {
    let mut stats = x402::FacilitatorStats {
        slot: settlements.slot,
        ..Default::default()
    };
    for delta in volume_deltas.deltas {
        let facilitator = delta.key.clone();
        let total_settlements = count_store.get_last(&facilitator).unwrap_or(0) as u64;
        let total_fees = fees_store
            .get_last(&facilitator)
            .map(|v| v.to_string())
            .unwrap_or_else(|| "0".to_string());
        let first_settlement_at = first_seen_store
            .get_last(format!("facilitator:{}", facilitator))
            .map(|secs| prost_types::Timestamp { seconds: secs, nanos: 0 });
        stats.stats.push(x402::FacilitatorStat {
            facilitator_address: facilitator,
            total_settlements,
            total_volume_settled: delta.new_value.to_string(),
            total_fees_spent: total_fees,
            first_settlement_at,
            last_settlement_at: settlements.block_timestamp.clone(),
        });
    }
    Ok(stats)
}

// =============================================
// LAYER 4: SQL Sink
// =============================================

#[substreams::handlers::map]
fn db_out(
    params: String,
    settlements: x402::Settlements,
    payer_stats: x402::PayerStats,
    recipient_stats: x402::RecipientStats,
    facilitator_stats: x402::FacilitatorStats,
) -> Result<DatabaseChanges, substreams::errors::Error> {
    let mut tables = Tables::new();

    let min_amount = params
        .split('=')
        .nth(1)
        .map(|v| v.to_string())
        .and_then(|v| BigInt::try_from(&v).ok())
        .unwrap_or_else(BigInt::zero);

    for s in settlements.settlements {
        let amount = BigInt::try_from(&s.amount).unwrap_or_else(|_| BigInt::zero());
        if amount < min_amount {
            continue;
        }
        let timestamp = s
            .timestamp
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());

        tables
            .create_row("settlements", &s.id)
            .set("slot", s.slot)
            .set("block_timestamp", &timestamp)
            .set("signature", &s.signature)
            .set("instruction_index", s.instruction_index as i64)
            .set("payer", &s.payer)
            .set("recipient", &s.recipient)
            .set("destination_ata", &s.destination_ata)
            .set("mint", &s.mint)
            .set("amount", &s.amount)
            .set("decimals", s.decimals as i64)
            .set("token_program", &s.token_program)
            .set("facilitator", &s.facilitator)
            .set("fee_lamports", &s.fee_lamports)
            .set("memo", &s.memo);
    }

    for stat in payer_stats.stats {
        let first_ts = stat
            .first_payment_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        let last_ts = stat
            .last_payment_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        tables
            .create_row("payers", &stat.payer_address)
            .set("total_spent", stat.total_spent.as_str())
            .set("total_payments", stat.total_payments as i64)
            .set("first_payment_at", &first_ts)
            .set("last_payment_at", &last_ts);
    }

    for stat in recipient_stats.stats {
        let first_ts = stat
            .first_payment_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        let last_ts = stat
            .last_payment_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        tables
            .create_row("recipients", &stat.recipient_address)
            .set("total_received", stat.total_received.as_str())
            .set("total_payments", stat.total_payments as i64)
            .set("first_payment_at", &first_ts)
            .set("last_payment_at", &last_ts);
    }

    for stat in facilitator_stats.stats {
        let first_ts = stat
            .first_settlement_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        let last_ts = stat
            .last_settlement_at
            .as_ref()
            .map(|t| unix_to_timestamp(t.seconds))
            .unwrap_or_else(|| "1970-01-01 00:00:00".to_string());
        tables
            .create_row("facilitators", &stat.facilitator_address)
            .set("total_settlements", stat.total_settlements as i64)
            .set("total_volume_settled", stat.total_volume_settled.as_str())
            .set("total_fees_spent", stat.total_fees_spent.as_str())
            .set("first_settlement_at", &first_ts)
            .set("last_settlement_at", &last_ts);
    }

    Ok(tables.to_database_changes())
}
