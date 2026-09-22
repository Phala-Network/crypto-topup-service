DROP INDEX refunds_confirmation_due_idx;
DROP INDEX refunds_idempotency_unique;

DROP TABLE refund_payment_claims;
DROP FUNCTION enforce_refund_payment_claim_capacity();

ALTER TABLE accounts
    DROP COLUMN status;

ALTER TABLE refunds
    DROP COLUMN tx_version,
    DROP COLUMN confirmed_at,
    DROP COLUMN updated_at,
    DROP COLUMN next_check_at,
    DROP COLUMN confirmation_evidence;
