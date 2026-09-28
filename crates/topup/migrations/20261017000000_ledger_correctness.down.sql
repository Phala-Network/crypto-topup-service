-- Restores the schema before 20261017000000_ledger_correctness. Refunds failed as dropped or
-- never seen become pending again with their transaction, as they were.

DROP INDEX deposits_unfinal_credited_idx;
ALTER TABLE accounts DROP COLUMN max_unfinalized_credit;

DROP INDEX deposits_unfinal_idx;
CREATE INDEX deposits_unfinal_idx
    ON deposits (chain_id, block_number)
    WHERE final_at IS NULL AND state <> 'reversed';
ALTER TABLE deposits DROP COLUMN finality_check_at;

UPDATE refunds SET status = 'pending', failure_reason = NULL
WHERE failure_reason IN ('transaction_dropped', 'transaction_not_found');
ALTER TABLE refunds
    DROP CONSTRAINT refunds_paid_at_check,
    DROP CONSTRAINT refunds_tx_origin_check,
    DROP COLUMN tx_nonce,
    DROP COLUMN tx_from,
    DROP COLUMN paid_at,
    DROP CONSTRAINT refunds_failure_reason_check,
    ADD CONSTRAINT refunds_failure_reason_check CHECK (
        (status = 'failed' AND failure_reason IN (
            'transaction_failed', 'transfer_not_found', 'sender_mismatch',
            'destination_mismatch', 'amount_mismatch', 'transfer_already_used'
        ))
        OR (status <> 'failed' AND failure_reason IS NULL)
    );
UPDATE refunds SET receipt_log_index = NULL WHERE status = 'pending';
ALTER TABLE refunds RENAME CONSTRAINT refunds_receipt_log_index_tx_hash_check
    TO refunds_log_index_tx_hash_check;
ALTER TABLE refunds RENAME CONSTRAINT refunds_receipt_log_index_check TO refunds_log_index_check;
ALTER TABLE refunds RENAME COLUMN receipt_log_index TO log_index;

COMMENT ON COLUMN refunds.log_index IS
    'Block-wide index of the Transfer log that pays the refund: named by mark_paid, or the matching log found at verification.';
