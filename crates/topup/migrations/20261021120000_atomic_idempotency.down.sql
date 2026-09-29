-- Restores the schema before 20261021120000_atomic_idempotency: the authorization table with the
-- grants it held, and idempotency keys without an owner.

CREATE TABLE permissions (
    permission text NOT NULL CHECK (permission ~ '^[a-z_]+\.(read|write)$'),
    principal text NOT NULL CONSTRAINT permissions_principal_check
        CHECK (principal IN ('key:secret', 'key:restricted')),
    PRIMARY KEY (permission, principal)
);
INSERT INTO permissions (permission, principal)
SELECT permission, principal
FROM unnest(ARRAY[
    'account.read', 'account.write', 'quotes.read', 'quotes.write', 'deposits.read',
    'deposits.write', 'deposit_addresses.read', 'deposit_addresses.write', 'refunds.read',
    'refunds.write', 'api_keys.read', 'api_keys.write', 'treasury.read', 'treasury.write',
    'endpoints.read', 'endpoints.write', 'events.read', 'sweeps.read', 'forwarders.read'
]) AS permission
CROSS JOIN unnest(ARRAY['key:secret', 'key:restricted']) AS principal
WHERE principal = 'key:secret'
   OR permission NOT IN ('api_keys.write', 'treasury.write', 'endpoints.write', 'account.write');
REVOKE INSERT, UPDATE, DELETE ON TABLE permissions FROM topup_app;

COMMENT ON COLUMN idempotency_keys.response IS
    'The first response, {"status", "body"}, replayed to repeats; NULL while that request runs. An API key''s secret is never stored.';
ALTER TABLE idempotency_keys DROP COLUMN owner;
