ALTER TABLE refunds
    DROP COLUMN route;

ALTER TABLE accounts
    DROP CONSTRAINT accounts_closed_at_status_check,
    DROP COLUMN closed_at;
