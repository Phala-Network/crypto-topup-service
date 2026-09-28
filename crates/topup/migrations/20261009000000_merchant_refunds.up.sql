-- Merchant refunds (docs/design/multi-tenant.md D5, §14, §16 PR 9). The merchant pays a refund
-- from the treasury of the deposit's own address and attaches the transaction; the service
-- verifies it at finality. The operator approve-and-record workflow goes: its statuses, the
-- `requested_by`/`approved_by` actors, the per-refund route, the hash versioning, and the
-- partial-claim table, which one refund per transfer log replaces.

DROP TRIGGER refund_payment_claim_capacity ON refund_payment_claims;
DROP FUNCTION enforce_refund_payment_claim_capacity();

ALTER TABLE refunds
    ADD COLUMN chain_id bigint,
    ADD COLUMN log_index bigint,
    ADD COLUMN failure_reason text;

UPDATE refunds
SET chain_id = deposit.chain_id
FROM deposits AS deposit
WHERE deposit.id = refunds.deposit_id;

-- A confirmed refund keeps the first transfer log it claimed.
UPDATE refunds
SET log_index = claim.log_index
FROM (
    SELECT refund_id, min(log_index) AS log_index
    FROM refund_payment_claims
    GROUP BY refund_id
) AS claim
WHERE claim.refund_id = refunds.id AND refunds.status = 'confirmed';

DROP TABLE refund_payment_claims;

-- Stripe's Refund statuses: a request not yet verified is `pending`.
ALTER TABLE refunds DROP CONSTRAINT refunds_status_check;
UPDATE refunds
SET status = CASE WHEN status = 'confirmed' THEN 'succeeded' ELSE 'pending' END;

DROP INDEX refunds_idempotency_unique;
DROP INDEX refunds_confirmation_due_idx;

ALTER TABLE refunds RENAME COLUMN to_address TO destination_address;
ALTER TABLE refunds RENAME CONSTRAINT refunds_to_address_canonical_hex_check
    TO refunds_destination_address_canonical_hex_check;

ALTER TABLE refunds
    DROP COLUMN requested_by,
    DROP COLUMN approved_by,
    DROP COLUMN confirmed_at,
    DROP COLUMN tx_version,
    DROP COLUMN route,
    ALTER COLUMN chain_id SET NOT NULL,
    ADD CONSTRAINT refunds_chain_id_check CHECK (chain_id >= 0),
    ADD CONSTRAINT refunds_log_index_check CHECK (log_index >= 0),
    DROP CONSTRAINT refunds_amount_atomic_check,
    ADD CONSTRAINT refunds_amount_atomic_check CHECK (amount_atomic > 0),
    ADD CONSTRAINT refunds_status_check
        CHECK (status IN ('pending', 'succeeded', 'failed', 'canceled')),
    ADD CONSTRAINT refunds_failure_reason_check CHECK (
        (status = 'failed' AND failure_reason IN (
            'transaction_failed', 'transfer_not_found', 'sender_mismatch',
            'destination_mismatch', 'amount_mismatch', 'transfer_already_used'
        ))
        OR (status <> 'failed' AND failure_reason IS NULL)
    ),
    ADD CONSTRAINT refunds_log_index_tx_hash_check CHECK (log_index IS NULL OR tx_hash IS NOT NULL),
    ADD CONSTRAINT refunds_succeeded_transfer_check
        CHECK (status <> 'succeeded' OR (tx_hash IS NOT NULL AND log_index IS NOT NULL));

-- One transfer log pays at most one refund. A failed or canceled refund holds no log, so the same
-- transfer can be attached to a new refund.
CREATE UNIQUE INDEX refunds_transfer_unique
    ON refunds (chain_id, tx_hash, log_index)
    WHERE log_index IS NOT NULL AND status IN ('pending', 'succeeded');
CREATE INDEX refunds_deposit_idx ON refunds (deposit_id, status);
CREATE INDEX refunds_verification_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'pending' AND tx_hash IS NOT NULL;

COMMENT ON COLUMN refunds.chain_id IS
    'Chain of the refunded deposit, where the merchant pays the refund.';
COMMENT ON COLUMN refunds.tx_hash IS
    'The merchant''s refund transaction, attached by mark_paid.';
COMMENT ON COLUMN refunds.log_index IS
    'Block-wide index of the Transfer log that pays the refund: named by mark_paid, or the matching log found at verification.';
COMMENT ON COLUMN refunds.failure_reason IS
    'Why verification failed; the refund''s reservation of the deposit is released.';
COMMENT ON COLUMN refunds.confirmation_evidence IS
    'Most recent finalized verification evidence of the attached transaction, including mismatches.';
COMMENT ON COLUMN refunds.next_check_at IS
    'Earliest time the verification worker reads the attached transaction again.';
