-- x402 Solana Pulse - PostgreSQL Schema
-- Version: 1.0.0
--
-- Detects x402 settlements via the SVM `exact` scheme:
-- https://github.com/coinbase/x402/blob/main/specs/schemes/exact/scheme_exact_svm.md
--
-- Run with: substreams-sink-sql setup "postgres://..." x402-solana-pulse-v1.0.0.spkg

-------------------------------------------------
-- SETTLEMENTS: Every x402 payment on Solana
-------------------------------------------------
CREATE TABLE IF NOT EXISTS settlements (
    id VARCHAR(128) PRIMARY KEY,              -- signature-instruction_index
    slot BIGINT NOT NULL,
    block_timestamp TIMESTAMP NOT NULL,
    signature VARCHAR(128) NOT NULL,          -- Base58 tx signature
    instruction_index BIGINT NOT NULL,

    -- Payment details (all base58 pubkeys)
    payer VARCHAR(64) NOT NULL,               -- TransferChecked authority
    recipient VARCHAR(64) NOT NULL,           -- Owner of destination ATA
    destination_ata VARCHAR(64) NOT NULL,
    mint VARCHAR(64) NOT NULL,                -- Token mint
    amount NUMERIC(38, 0) NOT NULL DEFAULT 0, -- Atomic units
    decimals BIGINT NOT NULL DEFAULT 0,

    -- Token program (spl-token or token-2022 program id)
    token_program VARCHAR(64) NOT NULL,

    -- Facilitator info
    facilitator VARCHAR(64) NOT NULL,         -- Transaction fee payer
    facilitator_name VARCHAR(64) NOT NULL DEFAULT '',  -- Enriched from x402scan allowlist
    facilitator_known BOOLEAN NOT NULL DEFAULT false,
    fee_lamports NUMERIC(20, 0) NOT NULL DEFAULT 0,

    -- Payment reference (Memo instruction data)
    memo TEXT,

    created_at TIMESTAMP DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_settlements_facilitator_name ON settlements(facilitator_name);
CREATE INDEX IF NOT EXISTS idx_settlements_unknown_facilitators ON settlements(facilitator) WHERE facilitator_known = false;

CREATE INDEX IF NOT EXISTS idx_settlements_slot ON settlements(slot);
CREATE INDEX IF NOT EXISTS idx_settlements_payer ON settlements(payer);
CREATE INDEX IF NOT EXISTS idx_settlements_recipient ON settlements(recipient);
CREATE INDEX IF NOT EXISTS idx_settlements_facilitator ON settlements(facilitator);
CREATE INDEX IF NOT EXISTS idx_settlements_mint ON settlements(mint);
CREATE INDEX IF NOT EXISTS idx_settlements_timestamp ON settlements(block_timestamp);
CREATE INDEX IF NOT EXISTS idx_settlements_amount ON settlements(amount DESC);

-------------------------------------------------
-- PAYERS: Aggregated stats per payer pubkey
-------------------------------------------------
CREATE TABLE IF NOT EXISTS payers (
    payer_address VARCHAR(64) PRIMARY KEY,
    total_spent NUMERIC(38, 0) NOT NULL DEFAULT 0,
    total_payments INTEGER NOT NULL DEFAULT 0,
    first_payment_at TIMESTAMP,
    last_payment_at TIMESTAMP,
    updated_at TIMESTAMP DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_payers_spent ON payers(total_spent DESC);
CREATE INDEX IF NOT EXISTS idx_payers_count ON payers(total_payments DESC);

-------------------------------------------------
-- RECIPIENTS: Resource servers receiving x402 payments
-------------------------------------------------
CREATE TABLE IF NOT EXISTS recipients (
    recipient_address VARCHAR(64) PRIMARY KEY,
    total_received NUMERIC(38, 0) NOT NULL DEFAULT 0,
    total_payments INTEGER NOT NULL DEFAULT 0,
    first_payment_at TIMESTAMP,
    last_payment_at TIMESTAMP,
    updated_at TIMESTAMP DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_recipients_revenue ON recipients(total_received DESC);
CREATE INDEX IF NOT EXISTS idx_recipients_count ON recipients(total_payments DESC);

-------------------------------------------------
-- FACILITATORS: Per-facilitator economics
-------------------------------------------------
CREATE TABLE IF NOT EXISTS facilitators (
    facilitator_address VARCHAR(64) PRIMARY KEY,
    total_settlements INTEGER NOT NULL DEFAULT 0,
    total_volume_settled NUMERIC(38, 0) NOT NULL DEFAULT 0,
    total_fees_spent NUMERIC(38, 0) NOT NULL DEFAULT 0,   -- lamports
    first_settlement_at TIMESTAMP,
    last_settlement_at TIMESTAMP,
    updated_at TIMESTAMP DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_facilitators_volume ON facilitators(total_volume_settled DESC);
CREATE INDEX IF NOT EXISTS idx_facilitators_settlements ON facilitators(total_settlements DESC);
CREATE INDEX IF NOT EXISTS idx_facilitators_fees ON facilitators(total_fees_spent DESC);

-------------------------------------------------
-- VIEWS: Protocol analytics dashboards
-------------------------------------------------

CREATE OR REPLACE VIEW daily_stats AS
SELECT
    block_timestamp::date AS date,
    SUM(amount) AS total_volume,
    COUNT(*) AS total_settlements,
    COUNT(DISTINCT payer) AS unique_payers,
    COUNT(DISTINCT recipient) AS unique_recipients,
    COUNT(DISTINCT facilitator) AS unique_facilitators,
    SUM(fee_lamports) AS total_lamport_fees
FROM settlements
GROUP BY block_timestamp::date
ORDER BY date DESC;

CREATE OR REPLACE VIEW top_payers AS
SELECT
    payer_address,
    total_spent,
    total_payments,
    CASE WHEN total_payments > 0 THEN total_spent / total_payments ELSE 0 END AS avg_payment_size,
    RANK() OVER (ORDER BY total_spent DESC) AS rank
FROM payers
WHERE total_payments >= 1
ORDER BY total_spent DESC
LIMIT 100;

CREATE OR REPLACE VIEW top_recipients AS
SELECT
    recipient_address,
    total_received,
    total_payments,
    CASE WHEN total_payments > 0 THEN total_received / total_payments ELSE 0 END AS avg_payment_received,
    RANK() OVER (ORDER BY total_received DESC) AS rank
FROM recipients
WHERE total_payments >= 1
ORDER BY total_received DESC
LIMIT 100;

CREATE OR REPLACE VIEW facilitator_economics AS
SELECT
    facilitator_address,
    total_settlements,
    total_volume_settled,
    total_fees_spent,
    CASE WHEN total_settlements > 0 THEN total_volume_settled / total_settlements ELSE 0 END AS avg_settlement_size,
    RANK() OVER (ORDER BY total_volume_settled DESC) AS rank
FROM facilitators
WHERE total_settlements >= 1
ORDER BY total_volume_settled DESC
LIMIT 100;

CREATE OR REPLACE VIEW whale_payments AS
SELECT
    s.*,
    p.total_spent AS payer_total_spent,
    p.total_payments AS payer_total_payments
FROM settlements s
LEFT JOIN payers p ON s.payer = p.payer_address
WHERE s.amount >= 100000000                   -- > $100 USDC (6 decimals)
ORDER BY s.block_timestamp DESC
LIMIT 500;

CREATE OR REPLACE VIEW recent_settlements AS
SELECT
    id,
    slot,
    block_timestamp,
    signature,
    payer,
    recipient,
    mint,
    amount,
    decimals,
    facilitator,
    fee_lamports,
    memo
FROM settlements
ORDER BY slot DESC, instruction_index DESC
LIMIT 100;
