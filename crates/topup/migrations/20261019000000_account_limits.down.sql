-- Restores the schema before 20261019000000_account_limits; a null cap takes the default the
-- service applied for it.
DROP INDEX quotes_open_exposure_idx;

COMMENT ON COLUMN account_limits.max_open_minor_customer IS NULL;
COMMENT ON COLUMN account_limits.max_open_minor_account IS NULL;
COMMENT ON TABLE account_limits IS NULL;

UPDATE account_limits SET
    max_open_quotes = COALESCE(max_open_quotes, CASE WHEN livemode THEN 1000 ELSE 100 END),
    max_open_minor_account =
        COALESCE(max_open_minor_account, CASE WHEN livemode THEN 5000000 ELSE 1000000 END),
    max_open_minor_customer = COALESCE(max_open_minor_customer, 500000);
ALTER TABLE account_limits ALTER COLUMN max_open_minor_customer SET NOT NULL;
ALTER TABLE account_limits ALTER COLUMN max_open_minor_account SET NOT NULL;
ALTER TABLE account_limits ALTER COLUMN max_open_quotes SET NOT NULL;
