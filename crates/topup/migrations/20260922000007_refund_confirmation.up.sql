ALTER TABLE refunds
    ADD COLUMN confirmation_evidence jsonb,
    ADD COLUMN next_check_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN confirmed_at timestamptz;

CREATE UNIQUE INDEX refunds_idempotency_unique
    ON refunds (deposit_id, to_address, amount_atomic);

CREATE INDEX refunds_confirmation_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'sent';

COMMENT ON COLUMN refunds.confirmation_evidence IS
    'Most recent finalized transaction verification evidence, including mismatches.';
COMMENT ON COLUMN refunds.next_check_at IS
    'Earliest time the background confirmation worker should inspect a sent refund.';
