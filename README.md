# x402 Solana Pulse

> Real-time payment protocol analytics for [Coinbase x402](https://github.com/coinbase/x402) on Solana

Track every x402 payment settlement on Solana. This Substreams detects when facilitators sponsor SPL Token `TransferChecked` settlements for [HTTP 402](https://docs.cdp.coinbase.com/x402) payments, extracting payer, recipient, mint, amount, facilitator, and memo from each transaction.

Companion to [x402-base-pulse](https://github.com/PaulieB14/x402-base-pulse) — same model, Solana native.

---

## How It Works

The [x402 protocol](https://docs.cdp.coinbase.com/x402/core-concepts/how-it-works) enables internet-native payments using the HTTP 402 status code. The Solana settlement path is defined by the [SVM `exact` scheme](https://github.com/coinbase/x402/blob/main/specs/schemes/exact/scheme_exact_svm.md):

1. Server responds with **HTTP 402** + payment requirements (mint, payTo, amount, feePayer)
2. Client constructs a transaction containing a `TransferChecked` to the resource server's ATA, signs it, and returns the partially-signed tx
3. Facilitator verifies the transaction, provides the final `feePayer` signature, and submits it to Solana
4. Solana executes an SPL Token (or Token-2022) `TransferChecked` + SPL Memo instruction
5. **This Substreams captures those transactions** by looking for the x402 SVM settlement shape

### Detection heuristic

Solana has no on-chain `FacilitatorRegistry`. Instead, x402 settlements are identified by three signals observed together in a transaction:

- Contains an SPL Token or Token-2022 `TransferChecked` instruction
- Contains an SPL Memo instruction (payment reference / nonce)
- Transaction `fee_payer` is distinct from the `TransferChecked` authority — i.e. the facilitator sponsors, the payer signs

## Modules

| Module | Kind | Description |
|--------|------|-------------|
| `map_x402_settlements` | Map | Detects x402 settlements in each Solana block and extracts payer / recipient / mint / amount / facilitator / memo |
| `store_payer_volume` | Store | Accumulates total amount spent per payer pubkey |
| `store_payer_count` | Store | Counts payments per payer |
| `store_recipient_volume` | Store | Accumulates total received per resource server (ATA owner) |
| `store_recipient_count` | Store | Counts payments per recipient |
| `store_facilitator_volume` | Store | Accumulates volume settled per facilitator |
| `store_facilitator_count` | Store | Counts settlements per facilitator |
| `store_facilitator_fees` | Store | Accumulates tx fees (lamports) paid per facilitator |
| `store_first_seen` | Store | Records first-seen block timestamp per payer / recipient / facilitator |
| `map_payer_stats` | Map | Computes payer leaderboards and averages |
| `map_recipient_stats` | Map | Computes resource server revenue stats |
| `map_facilitator_stats` | Map | Computes facilitator economics (volume, fees, counts) |
| `db_out` | Map | Outputs `DatabaseChanges` for the PostgreSQL sink |

## Programs Indexed

| Program | Pubkey |
|---------|--------|
| SPL Token | `TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA` |
| SPL Token-2022 | `TokenzQdBNbLqP5VEhdkAS6EPFLC1PHnBqCXEpPxuEb` |
| SPL Memo v2 | `MemoSq4gqABAXKb96qnH8TysNcWxMyWCqXgDLGmfcHr` |
| SPL Memo v1 | `Memo1UhkJRfHyvLMcVucJwxXeuD728EqVDDwQDxFMNo` |

## Quick Start

```bash
# Stream settlements
substreams run x402-solana-pulse map_x402_settlements \
  -e mainnet.sol.streamingfast.io:443 \
  -s 280000000 -t +1000

# GUI mode
substreams gui x402-solana-pulse map_x402_settlements \
  -e mainnet.sol.streamingfast.io:443 \
  -s 280000000

# Sink to PostgreSQL
substreams-sink-sql run "psql://localhost/x402" \
  x402-solana-pulse-v1.0.0.spkg \
  -e mainnet.sol.streamingfast.io:443
```

## SQL Output

### Tables
| Table | Key | Description |
|-------|-----|-------------|
| `settlements` | `signature-instruction_index` | Every settlement with payer, recipient, mint, amount, facilitator, fee, memo |
| `payers` | `payer_address` | Aggregated spend and payment count per payer pubkey |
| `recipients` | `recipient_address` | Revenue and payment count per resource server |
| `facilitators` | `facilitator_address` | Volume settled, settlement count, lamport fees per facilitator |

### Views
| View | Description |
|------|-------------|
| `daily_stats` | Daily protocol-wide volume, unique participants, fees |
| `top_payers` | Ranked by total spend |
| `top_recipients` | Ranked by total revenue |
| `facilitator_economics` | Volume settled vs fee cost per facilitator |
| `whale_payments` | Payments ≥ 100 USDC (atomic units) |
| `recent_settlements` | Latest 100 settlements |

## Build

```bash
cargo build --target wasm32-unknown-unknown --release
substreams pack substreams.yaml
```

## References

- [x402 Protocol](https://docs.cdp.coinbase.com/x402) — Coinbase's HTTP 402 payment standard
- [How It Works](https://docs.cdp.coinbase.com/x402/core-concepts/how-it-works) — Settlement flow
- [SVM exact scheme](https://github.com/coinbase/x402/blob/main/specs/schemes/exact/scheme_exact_svm.md) — Solana settlement spec
- [Network Support](https://docs.cdp.coinbase.com/x402/network-support) — Supported tokens and chains
- [x402 Source](https://github.com/coinbase/x402) — Protocol implementation
- [x402-base-pulse](https://github.com/PaulieB14/x402-base-pulse) — Companion Base substreams

## Network

- **Chain**: Solana mainnet
- **Endpoint**: `mainnet.sol.streamingfast.io:443`
- **Initial block**: 280,000,000 (adjustable)
