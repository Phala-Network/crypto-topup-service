ALTER TABLE accounts
    ADD COLUMN closed_at timestamptz,
    ADD CONSTRAINT accounts_closed_at_status_check CHECK (
        status = 'closed' OR closed_at IS NULL
    );

ALTER TABLE refunds
    ADD COLUMN route text;

UPDATE refunds AS refund
SET route = deposit.route
FROM deposits AS deposit
WHERE deposit.id = refund.deposit_id;

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM refunds WHERE route IS NULL) THEN
        RAISE EXCEPTION
            'migration 20260922000014 requires every existing refund to have a resolvable route';
    END IF;
END;
$$;

ALTER TABLE refunds
    ALTER COLUMN route SET NOT NULL;

COMMENT ON COLUMN accounts.closed_at IS
    'Workspace closure boundary; only deposits with block_time strictly after this value are late funds.';
COMMENT ON COLUMN refunds.route IS
    'Effective route selected when the refund was requested, including unsupported-asset fallback routing.';
