-- Restores the schema of 20261004000000_multi_tenant that the up migration changes. Removed rows
-- (request signing keys, role grants, per-object idempotency keys) are not recovered, and key and
-- account events are deleted.

DROP INDEX idempotency_keys_created_idx;

ALTER TABLE refunds ADD COLUMN idempotency_key text
    CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255);
CREATE UNIQUE INDEX refunds_idempotency_key_unique
    ON refunds (account_id, livemode, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
ALTER TABLE quotes ADD COLUMN idempotency_key text
    CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255);
CREATE UNIQUE INDEX quotes_idempotency_key_unique
    ON quotes (account_id, livemode, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

DELETE FROM webhook_deliveries
WHERE event_id IN (SELECT id FROM events WHERE object_type IN ('api_key', 'account'));
DELETE FROM events WHERE object_type IN ('api_key', 'account');
ALTER TABLE events DROP COLUMN actor;
ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote'));

DELETE FROM permissions
WHERE permission IN ('api_keys.read', 'api_keys.write')
   OR (permission IN ('treasury.read', 'treasury.write', 'account.write')
       AND principal IN ('key:secret', 'key:restricted'));
ALTER TABLE permissions DROP CONSTRAINT permissions_principal_check;
ALTER TABLE permissions ADD CONSTRAINT permissions_principal_check CHECK (principal IN (
    'role:owner', 'role:administrator', 'role:developer', 'role:view_only',
    'key:secret', 'key:restricted'
));

ALTER TABLE accounts DROP COLUMN due_diligence;
ALTER TABLE accounts DROP COLUMN contact;
ALTER TABLE accounts ADD COLUMN live_access boolean NOT NULL DEFAULT false;
UPDATE accounts SET live_access = charges_enabled;
ALTER TABLE accounts ADD COLUMN tos_acceptance jsonb;
ALTER TABLE accounts ADD COLUMN country text CHECK (country ~ '^[A-Z]{2}$');
ALTER TABLE accounts ADD COLUMN business_profile jsonb NOT NULL DEFAULT '{}';
ALTER TABLE accounts ADD CONSTRAINT accounts_charges_enabled_check
    CHECK (NOT charges_enabled OR live_access);

ALTER TABLE audit DROP CONSTRAINT audit_actor_type_check;
ALTER TABLE audit ADD CONSTRAINT audit_actor_type_check
    CHECK (actor_type IN ('user', 'api_key', 'admin', 'system'));

CREATE TABLE users (
    id uuid PRIMARY KEY,
    email text NOT NULL CHECK (char_length(email) BETWEEN 3 AND 320),
    name text,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX users_email_unique ON users (lower(email));

CREATE TABLE identities (
    user_id uuid NOT NULL REFERENCES users(id),
    provider text NOT NULL CHECK (provider IN ('google', 'github')),
    subject text NOT NULL CHECK (char_length(subject) BETWEEN 1 AND 255),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (provider, subject)
);
CREATE INDEX identities_user_idx ON identities (user_id);

CREATE TABLE passkeys (
    id uuid PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES users(id),
    credential_id bytea NOT NULL,
    public_key bytea NOT NULL,
    sign_count bigint NOT NULL DEFAULT 0 CHECK (sign_count >= 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT passkeys_credential_id_unique UNIQUE (credential_id)
);
CREATE INDEX passkeys_user_idx ON passkeys (user_id);

CREATE TABLE recovery_codes (
    user_id uuid NOT NULL REFERENCES users(id),
    code_hash bytea NOT NULL CHECK (octet_length(code_hash) = 32),
    used_at timestamptz,
    PRIMARY KEY (user_id, code_hash)
);

CREATE TABLE memberships (
    account_id uuid NOT NULL REFERENCES accounts(id),
    user_id uuid NOT NULL REFERENCES users(id),
    role text NOT NULL CHECK (role IN ('owner', 'administrator', 'developer', 'view_only')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, user_id)
);
CREATE INDEX memberships_user_idx ON memberships (user_id);

CREATE TABLE invitations (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    email text NOT NULL CHECK (char_length(email) BETWEEN 3 AND 320),
    role text NOT NULL CHECK (role IN ('owner', 'administrator', 'developer', 'view_only')),
    token_hash bytea NOT NULL CHECK (octet_length(token_hash) = 32),
    invited_by uuid NOT NULL REFERENCES users(id),
    expires_at timestamptz NOT NULL,
    accepted_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT invitations_token_hash_unique UNIQUE (token_hash)
);
CREATE INDEX invitations_account_idx ON invitations (account_id);

CREATE TABLE sessions (
    id_hash bytea PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    user_id uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    stepped_up_at timestamptz,
    expires_at timestamptz NOT NULL
);
CREATE INDEX sessions_user_idx ON sessions (user_id);

ALTER TABLE treasuries DROP COLUMN created_by;
ALTER TABLE treasuries ADD COLUMN created_by uuid REFERENCES users(id);
ALTER TABLE api_keys DROP COLUMN created_by;
ALTER TABLE api_keys ADD COLUMN created_by uuid REFERENCES users(id);

CREATE TABLE request_signing_keys (
    account_id uuid PRIMARY KEY REFERENCES accounts(id),
    livemode boolean NOT NULL,
    public_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
