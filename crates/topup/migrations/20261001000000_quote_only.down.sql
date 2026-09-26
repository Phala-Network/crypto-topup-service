-- Restores the schema only: a removed `addresses` pause is not restored, and every persistent
-- address counts as requested now.
ALTER TABLE route_pauses
    DROP CONSTRAINT route_pauses_scopes_check,
    ADD CONSTRAINT route_pauses_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    );
ALTER TABLE accounts
    DROP CONSTRAINT accounts_paused_scopes_check,
    ADD CONSTRAINT accounts_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    );
ALTER TABLE products
    DROP CONSTRAINT products_paused_scopes_check,
    ADD CONSTRAINT products_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    );

COMMENT ON COLUMN addresses.kind IS NULL;

ALTER TABLE addresses ADD COLUMN requested_at timestamptz NOT NULL DEFAULT now();
CREATE UNIQUE INDEX addresses_one_active_persistent_per_account_chain
    ON addresses (account_id, chain_id)
    WHERE kind = 'persistent' AND retired_at IS NULL;
CREATE INDEX addresses_persistent_requested_idx
    ON addresses (chain_id, requested_at)
    WHERE kind = 'persistent';
