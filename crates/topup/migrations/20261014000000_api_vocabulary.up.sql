-- API vocabulary and SDKs (docs/design/multi-tenant.md §12, §15, §16 PR 10).

-- A merchant pauses and resumes its own `quotes` (design §12). Kept apart from the operator's
-- `paused_scopes` so a merchant's resume never lifts an operator pause; both apply.
ALTER TABLE accounts ADD COLUMN self_paused_scopes text[] NOT NULL DEFAULT '{}'
    CONSTRAINT accounts_self_paused_scopes_check CHECK (self_paused_scopes <@ ARRAY['quotes']::text[]);
COMMENT ON COLUMN accounts.self_paused_scopes IS
    'Scopes the merchant paused through POST /v1/account/pause; only quotes. The operator''s pauses are paused_scopes.';

-- A deposit address's `client_secret`s: the payer's page reads the address's public view with one,
-- as a quote's page does. Each create or rotation response carries a new one; only its SHA-256 is
-- stored, and the newest few per address stay valid so several open pages keep working.
CREATE TABLE deposit_address_client_secrets (
    secret_hash bytea PRIMARY KEY CHECK (octet_length(secret_hash) = 32),
    deposit_address_id uuid NOT NULL REFERENCES deposit_addresses(id),
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX deposit_address_client_secrets_address_idx
    ON deposit_address_client_secrets (deposit_address_id, created_at);
-- Secrets are issued and pruned, never changed.
REVOKE UPDATE ON TABLE deposit_address_client_secrets FROM topup_app;
COMMENT ON TABLE deposit_address_client_secrets IS
    'SHA-256 of the client secrets that read a deposit address''s public view; scoped through deposit_addresses.';

-- The address export (GET /v1/addresses) is a read of the account's own data; `sweeps.read`
-- exists since the base schema.
INSERT INTO permissions (permission, principal)
VALUES ('addresses.read', 'key:secret'), ('addresses.read', 'key:restricted');
