DROP INDEX refunds_confirmation_due_idx;
DROP INDEX refunds_idempotency_unique;

ALTER TABLE refunds
    DROP COLUMN confirmed_at,
    DROP COLUMN updated_at,
    DROP COLUMN next_check_at,
    DROP COLUMN confirmation_evidence;
