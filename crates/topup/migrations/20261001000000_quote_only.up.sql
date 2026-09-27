-- Quote-first only (docs/architecture.md §9). Persistent addresses are no longer
-- issued. Existing ones stay as history and for custody: the finalized scanner still watches them
-- and credits late payments at spot, the flusher sweeps them by their stored salt, and the
-- reconciler checks them. Only the head scan's display-only watch of them, and the `addresses`
-- pause scope that stopped their issuance, go away.
DROP INDEX addresses_persistent_requested_idx;
DROP INDEX addresses_one_active_persistent_per_account_chain;
ALTER TABLE addresses DROP COLUMN requested_at;

COMMENT ON COLUMN addresses.kind IS
    'lock: a quote''s single-use address. persistent: an address issued before quotes were the only flow; kept for history and custody, never issued again.';

UPDATE products SET paused_scopes = array_remove(paused_scopes, 'addresses')
WHERE 'addresses' = ANY (paused_scopes);
UPDATE accounts SET paused_scopes = array_remove(paused_scopes, 'addresses')
WHERE 'addresses' = ANY (paused_scopes);
UPDATE route_pauses SET paused_scopes = array_remove(paused_scopes, 'addresses')
WHERE 'addresses' = ANY (paused_scopes);

ALTER TABLE products
    DROP CONSTRAINT products_paused_scopes_check,
    ADD CONSTRAINT products_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    );
ALTER TABLE accounts
    DROP CONSTRAINT accounts_paused_scopes_check,
    ADD CONSTRAINT accounts_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    );
ALTER TABLE route_pauses
    DROP CONSTRAINT route_pauses_scopes_check,
    ADD CONSTRAINT route_pauses_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    );
