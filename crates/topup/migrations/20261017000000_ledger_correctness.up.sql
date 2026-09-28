-- Ledger correctness (docs/design/multi-tenant.md, "Ledger correctness" amendment).
--
-- Refunds name their paying log by its position in the transaction's receipt, which survives
-- re-inclusion as a deposit's identity does, instead of the block-wide log index. A refund with a
-- transaction attached is no longer canceled: it ends only when verified, or failed once the
-- transaction is proven dropped (its sender's nonce consumed by another at finality) or never
-- seen, for which the transaction's sender and nonce are kept when first read.
ALTER TABLE refunds RENAME COLUMN log_index TO receipt_log_index;
ALTER TABLE refunds RENAME CONSTRAINT refunds_log_index_check TO refunds_receipt_log_index_check;
ALTER TABLE refunds RENAME CONSTRAINT refunds_log_index_tx_hash_check
    TO refunds_receipt_log_index_tx_hash_check;
-- A pending refund's named log was block-wide; any matching log now pays it.
UPDATE refunds SET receipt_log_index = NULL WHERE status = 'pending';

ALTER TABLE refunds
    ADD COLUMN paid_at timestamptz,
    ADD COLUMN tx_from text CHECK (tx_from ~ '^0x[0-9a-f]{40}$'),
    ADD COLUMN tx_nonce numeric(20,0) CHECK (tx_nonce >= 0),
    ADD CONSTRAINT refunds_tx_origin_check CHECK ((tx_from IS NULL) = (tx_nonce IS NULL)),
    DROP CONSTRAINT refunds_failure_reason_check,
    ADD CONSTRAINT refunds_failure_reason_check CHECK (
        (status = 'failed' AND failure_reason IN (
            'transaction_failed', 'transfer_not_found', 'sender_mismatch',
            'destination_mismatch', 'amount_mismatch', 'transfer_already_used',
            'transaction_dropped', 'transaction_not_found'
        ))
        OR (status <> 'failed' AND failure_reason IS NULL)
    );
UPDATE refunds SET paid_at = updated_at WHERE tx_hash IS NOT NULL;
ALTER TABLE refunds ADD CONSTRAINT refunds_paid_at_check CHECK ((tx_hash IS NULL) = (paid_at IS NULL));

COMMENT ON COLUMN refunds.receipt_log_index IS
    'Position of the paying Transfer log among the logs of the transaction''s receipt: named by mark_paid, or the matching log found at verification. Unlike the block-wide index it survives re-inclusion.';
COMMENT ON COLUMN refunds.paid_at IS
    'When mark_paid attached the transaction; the refund can no longer be canceled.';
COMMENT ON COLUMN refunds.tx_from IS
    'Sender of the attached transaction, kept when a provider first returns it; with tx_nonce, proves the transaction dropped once another consumed its nonce at finality.';

-- The finality watch gives each deposit it could not settle its own recheck time, so deposits it
-- keeps waiting on cannot starve later ones.
ALTER TABLE deposits ADD COLUMN finality_check_at timestamptz;
DROP INDEX deposits_unfinal_idx;
CREATE INDEX deposits_unfinal_idx
    ON deposits (chain_id, block_number, id)
    WHERE final_at IS NULL AND state <> 'reversed';
COMMENT ON COLUMN deposits.finality_check_at IS
    'Earliest time the finality watch reads the deposit again; null when it was never read.';

-- Exposure to reversal: the credit of an account's credited deposits that are not final yet, per
-- mode, is capped. A deposit that would pass the cap is credited once it is final.
ALTER TABLE accounts ADD COLUMN max_unfinalized_credit bigint NOT NULL DEFAULT 100000
    CHECK (max_unfinalized_credit >= 0);
COMMENT ON COLUMN accounts.max_unfinalized_credit IS
    'Cap, in cents and per mode, on the credit of the account''s credited deposits that are not final yet; set by the operator.';
CREATE INDEX deposits_unfinal_credited_idx
    ON deposits (account_id, livemode)
    WHERE state = 'credited' AND final_at IS NULL;
