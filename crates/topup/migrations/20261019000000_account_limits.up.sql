-- Open-quote caps per account and mode (docs/design/multi-tenant.md §12), read from
-- `account_limits` instead of the route file. Every cap is optional per account: null keeps the
-- service default of the mode (`crate::limits`), as `max_active_deposit_addresses` already does.
ALTER TABLE account_limits ALTER COLUMN max_open_quotes DROP NOT NULL;
ALTER TABLE account_limits ALTER COLUMN max_open_minor_account DROP NOT NULL;
ALTER TABLE account_limits ALTER COLUMN max_open_minor_customer DROP NOT NULL;

COMMENT ON TABLE account_limits IS
    'The operator''s caps per account and mode; a null column, or no row, keeps the mode''s default.';
COMMENT ON COLUMN account_limits.max_open_minor_account IS
    'Cap on the credit, in cents, of the account''s open quotes in the mode.';
COMMENT ON COLUMN account_limits.max_open_minor_customer IS
    'Cap on the credit, in cents, of one customer''s open quotes.';

-- The exposure check sums the open reserved quotes of one account and mode.
CREATE INDEX quotes_open_exposure_idx ON quotes (account_id, livemode)
    WHERE status = 'open' AND exposure_reserved;
