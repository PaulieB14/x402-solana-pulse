# x402 Solana Pulse

> Real-time payment protocol analytics for [Coinbase x402](https://github.com/coinbase/x402) on Solana

Track every x402 payment settlement on Solana — **both schemes**. This Substreams detects facilitator-sponsored settlements for [HTTP 402](https://docs.cdp.coinbase.com/x402) payments and extracts payer, recipient, mint, amount, facilitator and memo from each one.

| Scheme | Shape on chain | Covered |
|---|---|---|
| `exact` | SPL Token `TransferChecked` at top level | since v1.0 |
| `upto` | [payment-channels](https://github.com/coinbase/x402) program, transfer as a CPI | **new in v1.2** |

Every row carries `scheme`, so you can filter to one or trust the total. That matters more than it
sounds: `upto` settlements have no top-level `TransferChecked` at all, so any Solana x402 indexer
that only walks top-level instructions silently undercounts them and looks complete while doing it.

Companion to [x402-base-pulse](https://github.com/PaulieB14/x402-base-pulse) — same model, Solana native.

---

## How It Works

The [x402 protocol](https://docs.cdp.coinbase.com/x402/core-concepts/how-it-works) enables internet-native payments using the HTTP 402 status code. The Solana settlement path is defined by the [SVM `exact` scheme](https://github.com/coinbase/x402/blob/main/specs/schemes/exact/scheme_exact_svm.md):

1. Server responds with **HTTP 402** + payment requirements (mint, payTo, amount, feePayer)
2. Client constructs a transaction containing a `TransferChecked` to the resource server's ATA, signs it, and returns the partially-signed tx
3. Facilitator verifies the transaction, provides the final `feePayer` signature, and submits it to Solana
4. Solana executes an SPL Token (or Token-2022) `TransferChecked` + SPL Memo instruction
5. **This Substreams captures those transactions** by looking for the x402 SVM settlement shape

### The `upto` scheme is shaped differently

`upto` settles through the payment-channels program
(`CHNLxYvVA28MJP9PrFuDXccuoGXAx7jBacfLEkahyGsX`) rather than a direct transfer. Observed on
mainnet, the transaction looks like:

```
top-level : [ComputeBudget, ComputeBudget, PAYMENT_CHANNELS, Memo]
inner CPI : SPL TransferChecked     <- the actual token movement
```

Two consequences worth knowing before you query:

- **The transfer is a CPI**, so it never appears among top-level instructions. A detector that only
  walks those will report zero `upto` volume rather than an error.
- **The inner transfer's authority is the channel PDA, not the end payer.** Those rows set
  `scheme='upto'` and carry the PDA in both `payer` and `channel`, so you always know which one you
  are holding instead of mistaking a channel for a wallet.

### Detection: strict spec match + known-facilitator fallback

Solana has no on-chain `FacilitatorRegistry`. This substreams uses a two-layer approach:

**1. Strict spec match (any facilitator, known or new):** Matches the SVM `exact` scheme layout verbatim — 3–6 top-level instructions consisting of ComputeBudget SetLimit, ComputeBudget SetPrice, TransferChecked, then optional Memo / Lighthouse. Fee payer must differ from the TransferChecked authority. This captures **any** spec-compliant facilitator automatically, including new ones that haven't been added to the allowlist.

**2. Relaxed match for known facilitators:** For the 21 facilitator pubkeys mirrored from [x402scan's registry](https://github.com/Merit-Systems/x402scan/tree/main/packages/external/facilitators/src/facilitators), we accept looser layouts (TransferChecked + Memo anywhere in top-level instructions) so non-spec variants still get indexed. Known facilitators include: Coinbase, PayAI, Dexter, Daydreams, OpenX402, AnySpend, CodeNut, Corbits, Aurracloud, Bitrefill, Cascade, OpenFacilitator, Relai, UltravioletaDAO, X402Jobs.

### Splitting volume by scheme

```sql
SELECT scheme, COUNT(*) AS settlements, SUM(amount::numeric) AS volume
FROM settlements
GROUP BY scheme;
```

Channel activity, for `upto` only:

```sql
SELECT channel, COUNT(*) AS settlements, SUM(amount::numeric) AS volume
FROM settlements
WHERE scheme = 'upto'
GROUP BY channel
ORDER BY volume DESC;
```

### Discovering new facilitators

Every settlement has `facilitator_name` (empty if unknown) and `facilitator_known` (boolean). To find new facilitators to add to the allowlist:

```sql
SELECT facilitator, COUNT(*) AS tx_count, SUM(amount) AS volume
FROM settlements
WHERE facilitator_known = false
GROUP BY facilitator
ORDER BY tx_count DESC
LIMIT 20;
```

High-volume unknown fee-payers are candidates. Verify on Solscan, then add to `KNOWN_FACILITATORS` in [src/lib.rs](src/lib.rs) and publish a new version.

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
