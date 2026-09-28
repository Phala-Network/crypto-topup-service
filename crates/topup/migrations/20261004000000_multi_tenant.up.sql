-- Multi-tenant schema (docs/design/multi-tenant.md §14; docs/architecture.md §6). The pre-tenancy
-- history is squashed into this migration: staging is reset (a HUMAN-ONLY step of design PR 13),
-- so it runs only on an empty database and migrates no data. The service connects through a
-- login role that is a member of `topup_app`; migrations run as the trusted database owner.
--
-- Tenant tables carry `account_id` and `livemode`, and every merchant query is built from a
-- server-side scope of both (design D13). Composite foreign keys make each row agree with its
-- parent's account and mode, so no row can join another tenant's rows. Tables without
-- `account_id` are reached only through a scoped parent.
--
-- Three groups of tables belong to flows that later design PRs replace and are kept until then:
-- `flushes`, `flushed`, `flush_exclusions`, and `deposits.flush_id` (the operator flusher, PR 4),
-- `request_signing_keys` (RFC 9421 merchant requests, PR 6), and the refund workflow columns
-- (PR 10).

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_app') THEN
        CREATE ROLE topup_app NOLOGIN;
    END IF;
END;
$$;

GRANT USAGE ON SCHEMA public TO topup_app;

-- Every table below gets the operational grant; append-only and read-only tables narrow it after
-- they are created. No application table grants TRUNCATE.
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO topup_app;

-- The SQLx migrator creates its history table before this migration runs, so the default above
-- does not cover it. The service reads it to refuse a schema it was not built for.
GRANT SELECT ON TABLE _sqlx_migrations TO topup_app;

CREATE FUNCTION reject_append_only_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = '55000';
END;
$$;

COMMENT ON FUNCTION reject_append_only_mutation() IS
    'Defense-in-depth append-only enforcement; trusted table owners and superusers can disable or bypass triggers.';

-- Accounts (the tenant), their policies and limits.

CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    public_id text GENERATED ALWAYS AS ('acct_' || replace(id::text, '-', '')) STORED,
    name text NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    business_profile jsonb NOT NULL DEFAULT '{}',
    country text CHECK (country ~ '^[A-Z]{2}$'),
    tos_acceptance jsonb,
    live_access boolean NOT NULL DEFAULT false,
    charges_enabled boolean NOT NULL DEFAULT false,
    restricted boolean NOT NULL DEFAULT false,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    webhook_key_version jsonb NOT NULL DEFAULT '{"live": 1, "test": 1}',
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT accounts_public_id_unique UNIQUE (public_id),
    CONSTRAINT accounts_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    ),
    -- Live mode opens only to accounts the operator granted live access (design D12).
    CONSTRAINT accounts_charges_enabled_check CHECK (NOT charges_enabled OR live_access),
    CONSTRAINT accounts_webhook_key_version_check CHECK (
        jsonb_typeof(webhook_key_version -> 'live') = 'number'
        AND jsonb_typeof(webhook_key_version -> 'test') = 'number'
    )
);

COMMENT ON COLUMN accounts.public_id IS
    'The account''s API id, acct_ and the 32 hex digits of id.';
COMMENT ON COLUMN accounts.paused_scopes IS
    'Operator and self-serve pauses of the whole account (design §12); flush is removed with the flusher.';

-- An account may require a stricter confirmation than a route's floor, never a weaker one
-- (design D1). Without a row the route's value applies.
CREATE TABLE confirmation_policies (
    account_id uuid NOT NULL REFERENCES accounts(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    required text NOT NULL CHECK (
        required IN ('safe', 'finalized') OR required ~ '^[1-9][0-9]{0,5}$'
    ),
    PRIMARY KEY (account_id, chain_id)
);

-- Caps per account and mode (design §12). Without a row the defaults apply.
CREATE TABLE account_limits (
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    max_open_quotes integer NOT NULL CHECK (max_open_quotes > 0),
    max_open_minor_account bigint NOT NULL CHECK (max_open_minor_account >= 0),
    max_open_minor_customer bigint NOT NULL CHECK (max_open_minor_customer >= 0),
    PRIMARY KEY (account_id, livemode)
);

-- People: users, their login identities and passkeys, and memberships (design D6, D8).

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

-- Server-side dashboard sessions; only a hash of the session id is stored.
CREATE TABLE sessions (
    id_hash bytea PRIMARY KEY CHECK (octet_length(id_hash) = 32),
    user_id uuid NOT NULL REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    last_seen_at timestamptz NOT NULL DEFAULT now(),
    stepped_up_at timestamptz,
    expires_at timestamptz NOT NULL
);

CREATE INDEX sessions_user_idx ON sessions (user_id);

-- The one authorization table (design D13): each row grants a permission to a role or a key
-- kind. Roles and API keys are checked against the same rows. The migration owns its contents.
CREATE TABLE permissions (
    permission text NOT NULL CHECK (permission ~ '^[a-z_]+\.(read|write)$'),
    principal text NOT NULL CHECK (principal IN (
        'role:owner', 'role:administrator', 'role:developer', 'role:view_only',
        'key:secret', 'key:restricted'
    )),
    PRIMARY KEY (permission, principal)
);

-- Secret keys hold every API permission and no dashboard permission; a restricted key may be
-- granted any of the same API permissions. Roles follow Stripe's: view_only reads; developer
-- also writes keys, endpoints, and refunds; administrator holds everything but ownership; the
-- owner holds everything.
INSERT INTO permissions (permission, principal)
SELECT permission, principal
FROM (VALUES
    -- permission,        API,   view_only, developer, administrator
    ('account.read',      true,  true,  true,  true),
    ('quotes.read',       true,  true,  true,  true),
    ('deposits.read',     true,  true,  true,  true),
    ('refunds.read',      true,  true,  true,  true),
    ('sweeps.read',       true,  true,  true,  true),
    ('events.read',       true,  true,  true,  true),
    ('endpoints.read',    true,  true,  true,  true),
    ('treasury.read',     false, true,  true,  true),
    ('keys.read',         false, true,  true,  true),
    ('members.read',      false, true,  true,  true),
    ('audit.read',        false, true,  true,  true),
    ('quotes.write',      true,  false, false, true),
    ('refunds.write',     true,  false, true,  true),
    ('endpoints.write',   true,  false, true,  true),
    ('keys.write',        false, false, true,  true),
    ('treasury.write',    false, false, false, true),
    ('members.write',     false, false, false, true),
    ('account.write',     false, false, false, true),
    ('activation.write',  false, false, false, true),
    ('ownership.write',   false, false, false, false)
) AS grants (permission, api, view_only, developer, administrator)
CROSS JOIN LATERAL (VALUES
    ('key:secret', api),
    ('key:restricted', api),
    ('role:view_only', view_only),
    ('role:developer', developer),
    ('role:administrator', administrator),
    ('role:owner', true)
) AS holders (principal, holds)
WHERE holds;

-- API keys (design D7): only a SHA-256 of the key is stored.
CREATE TABLE api_keys (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    kind text NOT NULL CHECK (kind IN ('secret', 'restricted')),
    name text NOT NULL DEFAULT '' CHECK (char_length(name) <= 200),
    permissions jsonb,
    prefix text NOT NULL CHECK (prefix ~ '^ppay_(sk|rk)_(live|test)_$'),
    last4 text NOT NULL CHECK (last4 ~ '^[0-9A-Za-z]{4}$'),
    key_hash bytea NOT NULL CHECK (octet_length(key_hash) = 32),
    created_by uuid REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now(),
    expires_at timestamptz,
    last_used_at timestamptz,
    revoked_at timestamptz,
    CONSTRAINT api_keys_key_hash_unique UNIQUE (key_hash),
    CONSTRAINT api_keys_permissions_check CHECK ((kind = 'restricted') = (permissions IS NOT NULL)),
    CONSTRAINT api_keys_prefix_kind_mode_check CHECK (
        prefix = 'ppay_' || CASE kind WHEN 'secret' THEN 'sk' ELSE 'rk' END || '_'
            || CASE WHEN livemode THEN 'live' ELSE 'test' END || '_'
    )
);

CREATE INDEX api_keys_account_idx ON api_keys (account_id, livemode);

-- Transitional until API keys replace RFC 9421 merchant requests (design PR 6): the ed25519
-- public key an account signs its requests with, key id `{accounts.public_id}/v1`, and the mode
-- the key selects.
CREATE TABLE request_signing_keys (
    account_id uuid PRIMARY KEY REFERENCES accounts(id),
    livemode boolean NOT NULL,
    public_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

-- Treasuries per account and chain (design D10). A route's chain fixes the mode.
CREATE TABLE treasuries (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    address text NOT NULL CHECK (
        address ~ '^0x[0-9a-f]{40}$' AND address <> '0x0000000000000000000000000000000000000000'
    ),
    proof_message text NOT NULL,
    proof_signature text NOT NULL,
    verified_at timestamptz,
    effective_at timestamptz,
    canceled_at timestamptz,
    screened_at timestamptz,
    created_by uuid REFERENCES users(id),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX treasuries_account_chain_idx ON treasuries (account_id, chain_id, effective_at);

-- Pauses and replay protection.

CREATE TABLE route_pauses (
    route text PRIMARY KEY,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT route_pauses_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    )
);

CREATE TABLE seen_signatures (
    kid text NOT NULL,
    signature_hash bytea NOT NULL,
    created timestamptz NOT NULL,
    PRIMARY KEY (kid, signature_hash)
);

CREATE INDEX seen_signatures_created_idx ON seen_signatures (created);

-- A merchant's end customer, named by the merchant's `client_reference_id` (design D6).
CREATE TABLE customers (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    client_reference_id text NOT NULL CHECK (char_length(client_reference_id) BETWEEN 1 AND 200),
    paused_scopes text[] NOT NULL DEFAULT '{}',
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT customers_reference_unique UNIQUE (account_id, livemode, client_reference_id),
    CONSTRAINT customers_scope_unique UNIQUE (id, account_id, livemode),
    CONSTRAINT customers_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
    )
);

-- Quotes (architecture §9): a locked price and exact amount for one single-use address.

CREATE TABLE quotes (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    customer_id uuid NOT NULL,
    route text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    price_scaled numeric(78,0) NOT NULL CHECK (price_scaled >= 0),
    expires_at timestamptz NOT NULL,
    consumed_by uuid,
    credit_minor numeric(78,0) NOT NULL CHECK (credit_minor >= 0),
    status text NOT NULL DEFAULT 'open',
    exposure_reserved boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    closed_at timestamptz,
    idempotency_key text
        CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255),
    client_secret_hash bytea
        CHECK (client_secret_hash IS NULL OR octet_length(client_secret_hash) = 32),
    CONSTRAINT quotes_consumed_by_unique UNIQUE (consumed_by),
    CONSTRAINT quotes_scope_unique UNIQUE (id, account_id, livemode),
    CONSTRAINT quotes_customer_fkey FOREIGN KEY (customer_id, account_id, livemode)
        REFERENCES customers (id, account_id, livemode),
    CONSTRAINT quotes_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$'),
    CONSTRAINT quotes_price_scaled_integer_check
        CHECK (price_scaled = trunc(price_scaled) AND price_scaled::text ~ '^[0-9]+$'),
    CONSTRAINT quotes_status_check
        CHECK (status IN ('open', 'consumed', 'expired', 'cancelled')),
    CONSTRAINT quotes_status_consumption_check CHECK (
        (status = 'consumed' AND consumed_by IS NOT NULL)
        OR (status <> 'consumed' AND consumed_by IS NULL)
    ),
    CONSTRAINT quotes_closed_at_check CHECK (
        (status = 'open' AND closed_at IS NULL)
        OR (status <> 'open' AND closed_at IS NOT NULL)
    )
);

CREATE INDEX quotes_open_expiry_idx ON quotes (expires_at, id) WHERE status = 'open';
CREATE INDEX quotes_customer_idx ON quotes (customer_id, created_at);
CREATE UNIQUE INDEX quotes_idempotency_key_unique
    ON quotes (account_id, livemode, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

COMMENT ON COLUMN quotes.idempotency_key IS
    'The Idempotency-Key the quote was created with; a repeat with the same parameters returns this quote.';
COMMENT ON COLUMN quotes.client_secret_hash IS
    'SHA-256 of the quote''s client_secret, the bearer of its public read; replaced when an Idempotency-Key repeat returns a new secret.';

-- Addresses and the chain scanner.

-- A quote's single-use forwarder address. `treasury` is the clone argument: the only address the
-- forwarder can pay (design D2, D3), fixed when the address is issued.
CREATE TABLE addresses (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    livemode boolean NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    quote_id uuid NOT NULL,
    salt text NOT NULL,
    treasury text NOT NULL,
    address text NOT NULL,
    created_block bigint NOT NULL DEFAULT 0 CHECK (created_block >= 0),
    backfilled boolean NOT NULL DEFAULT false,
    CONSTRAINT addresses_chain_address_unique UNIQUE (chain_id, address),
    CONSTRAINT addresses_quote_unique UNIQUE (quote_id),
    CONSTRAINT addresses_scope_unique UNIQUE (id, account_id, livemode),
    CONSTRAINT addresses_quote_fkey FOREIGN KEY (quote_id, account_id, livemode)
        REFERENCES quotes (id, account_id, livemode),
    CONSTRAINT addresses_salt_canonical_hex_check CHECK (salt ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT addresses_treasury_canonical_hex_check CHECK (
        treasury ~ '^0x[0-9a-f]{40}$' AND treasury <> '0x0000000000000000000000000000000000000000'
    ),
    CONSTRAINT addresses_address_canonical_hex_check CHECK (address ~ '^0x[0-9a-f]{40}$')
);

CREATE INDEX addresses_pending_backfill_idx
    ON addresses (chain_id, created_block, id)
    WHERE backfilled = false;

COMMENT ON COLUMN addresses.created_block IS
    'Earliest block the scanner must inspect for this counterfactual address; zero makes the first scanner pass check the full chain history.';
COMMENT ON COLUMN addresses.backfilled IS
    'True after the scanner transaction has covered created_block through the chain cursor.';

CREATE TABLE cursors (
    chain_id bigint PRIMARY KEY CHECK (chain_id >= 0),
    scanned_block bigint NOT NULL CHECK (scanned_block >= 0),
    scanned_block_time timestamptz,
    confirmed_block bigint CHECK (confirmed_block >= 0)
);

COMMENT ON COLUMN cursors.scanned_block IS
    'Last block the finalized scanner committed through. Quote expiry, reconciliation, and the display-only pending view read it.';
COMMENT ON COLUMN cursors.scanned_block_time IS
    'Block time of the finalized head when the scanner last committed through it; a lower bound on the time of scanned_block. Quotes expire only once this passes expires_at.';
COMMENT ON COLUMN cursors.confirmed_block IS
    'Last block the fast scanner committed through at the route confirmation (a depth or safe); null when the chain credits only at finalized.';

-- Display-only transfers seen by the head scan (architecture §8, §12). Rows never feed deposits,
-- transitions, quotes, exposure, or reconciliation; the finalized scanner deletes them in the
-- same transaction that advances its cursor past their block. Scoped through `addresses`.
CREATE TABLE pending_transfers (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    receipt_log_index bigint NOT NULL CHECK (receipt_log_index >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    block_time timestamptz NOT NULL,
    head_block bigint NOT NULL,
    address_id uuid NOT NULL REFERENCES addresses(id),
    asset_contract text NOT NULL,
    from_address text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, tx_hash, log_index),
    CONSTRAINT pending_transfers_head_check CHECK (head_block >= block_number)
);

CREATE INDEX pending_transfers_address_idx
    ON pending_transfers (address_id, block_number, log_index);
CREATE INDEX pending_transfers_chain_block_idx
    ON pending_transfers (chain_id, block_number);

COMMENT ON COLUMN pending_transfers.head_block IS
    'Provider A latest block at the last head scan that saw this transfer; confirmations = head_block - block_number + 1.';

-- Operator flushes, removed with the flusher (design PR 4).

CREATE TABLE flushes (
    id uuid PRIMARY KEY,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    token text NOT NULL,
    operator text NOT NULL,
    nonce numeric(78,0) NOT NULL CHECK (nonce >= 0),
    tx_hash text,
    block_number bigint CHECK (block_number >= 0),
    status text NOT NULL CHECK (status IN ('planned', 'sent', 'confirmed', 'reverted')),
    receipt jsonb,
    CONSTRAINT flushes_operator_nonce_unique UNIQUE (chain_id, operator, nonce),
    CONSTRAINT flushes_token_canonical_hex_check CHECK (token ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT flushes_operator_canonical_hex_check CHECK (operator ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT flushes_tx_hash_canonical_hex_check
        CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT flushes_nonce_integer_check
        CHECK (nonce = trunc(nonce) AND nonce::text ~ '^[0-9]+$')
);

CREATE TABLE flushed (
    flush_id uuid NOT NULL REFERENCES flushes(id),
    address_id uuid NOT NULL REFERENCES addresses(id),
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    log_index bigint NOT NULL CHECK (log_index >= 0),
    PRIMARY KEY (flush_id, address_id),
    CONSTRAINT flushed_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$')
);

CREATE TABLE flush_exclusions (
    chain_id bigint NOT NULL,
    token text NOT NULL,
    address_id uuid NOT NULL REFERENCES addresses(id),
    reason text NOT NULL,
    retry_after timestamptz NOT NULL,
    failures integer NOT NULL CHECK (failures > 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, token, address_id)
);

-- Deposits and their history.

CREATE TABLE deposits (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    livemode boolean NOT NULL,
    customer_id uuid NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    receipt_log_index bigint NOT NULL CHECK (receipt_log_index >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    block_time timestamptz NOT NULL,
    tx_from text CHECK (tx_from ~ '^0x[0-9a-f]{40}$'),
    tx_nonce numeric(20,0) CHECK (tx_nonce >= 0 AND tx_nonce = trunc(tx_nonce)),
    address_id uuid NOT NULL,
    route text,
    route_version bigint CHECK (route_version >= 0),
    asset_contract text NOT NULL,
    from_address text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    state text NOT NULL CHECK (
        state IN ('detected', 'confirmed', 'credited', 'swept', 'rejected', 'reversed')
    ),
    reason text CHECK (reason IN (
        'unsupported_asset', 'below_minimum', 'out_of_range', 'sanctioned', 'out_of_bounds'
    )),
    attempt integer NOT NULL DEFAULT 0 CHECK (attempt >= 0),
    next_attempt_at timestamptz NOT NULL,
    lease_token uuid,
    lease_until timestamptz,
    valuation_at timestamptz,
    price_scaled numeric(78,0) CHECK (price_scaled >= 0),
    price_source text CHECK (price_source IN ('spot', 'lock')),
    credit_minor numeric(78,0) CHECK (credit_minor >= 0),
    quote jsonb,
    flush_id uuid REFERENCES flushes(id),
    confirmations_at timestamptz NOT NULL DEFAULT now(),
    final_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT deposits_chain_event_unique UNIQUE (chain_id, tx_hash, receipt_log_index),
    CONSTRAINT deposits_scope_unique UNIQUE (id, account_id, livemode),
    CONSTRAINT deposits_address_fkey FOREIGN KEY (address_id, account_id, livemode)
        REFERENCES addresses (id, account_id, livemode),
    CONSTRAINT deposits_customer_fkey FOREIGN KEY (customer_id, account_id, livemode)
        REFERENCES customers (id, account_id, livemode),
    CONSTRAINT deposits_reason_state_check CHECK (
        (state = 'rejected' AND reason IS NOT NULL)
        OR (state <> 'rejected' AND reason IS NULL)
    ),
    CONSTRAINT deposits_lease_check CHECK (
        (lease_token IS NULL AND lease_until IS NULL)
        OR (lease_token IS NOT NULL AND lease_until IS NOT NULL)
    ),
    -- A deposit is swept only through a finalized `Flushed` event after its final log position.
    CONSTRAINT deposits_swept_final_check CHECK (state <> 'swept' OR final_at IS NOT NULL),
    CONSTRAINT deposits_tx_hash_canonical_hex_check CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT deposits_block_hash_canonical_hex_check CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT deposits_asset_contract_canonical_hex_check
        CHECK (asset_contract ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT deposits_from_address_canonical_hex_check
        CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT deposits_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$'),
    CONSTRAINT deposits_price_scaled_integer_check
        CHECK (
            price_scaled IS NULL
            OR (price_scaled = trunc(price_scaled) AND price_scaled::text ~ '^[0-9]+$')
        ),
    CONSTRAINT deposits_credit_minor_integer_check
        CHECK (
            credit_minor IS NULL
            OR (credit_minor = trunc(credit_minor) AND credit_minor::text ~ '^[0-9]+$')
        )
);

CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state NOT IN ('swept', 'rejected', 'reversed');

-- Quote expiry waits while a payment mined inside the window is still unconfirmed.
CREATE INDEX deposits_detected_address_idx ON deposits (address_id) WHERE state = 'detected';

-- The finality watch reads every deposit that is neither final nor reversed.
CREATE INDEX deposits_unfinal_idx
    ON deposits (chain_id, block_number)
    WHERE final_at IS NULL AND state <> 'reversed';

-- Merchant listings read one account and mode, newest first.
CREATE INDEX deposits_scope_created_idx ON deposits (account_id, livemode, created_at, id);

COMMENT ON COLUMN deposits.receipt_log_index IS
    'Position of the transfer log among the logs of its transaction''s receipt; with chain_id and tx_hash the deposit identity. log_index, block_number, and block_hash are evidence that follows re-inclusion.';
COMMENT ON COLUMN deposits.tx_from IS
    'Sender of the transaction; with tx_nonce, proves a dropped transaction once another consumed its nonce.';
COMMENT ON COLUMN deposits.confirmations_at IS
    'When the transfer reached the required confirmation and was recorded as a deposit.';
COMMENT ON COLUMN deposits.final_at IS
    'When both providers showed the transfer at or below finalized. Null while the deposit can still be reversed.';

ALTER TABLE quotes
    ADD CONSTRAINT quotes_consumed_by_fkey FOREIGN KEY (consumed_by) REFERENCES deposits(id);

-- Scoped through `deposits`.
CREATE TABLE transitions (
    id uuid PRIMARY KEY,
    deposit_id uuid NOT NULL REFERENCES deposits(id),
    from_state text NOT NULL CHECK (from_state IN (
        'detected', 'confirmed', 'credited', 'swept', 'rejected', 'reversed'
    )),
    to_state text NOT NULL CHECK (to_state IN (
        'detected', 'confirmed', 'credited', 'swept', 'rejected', 'reversed'
    )),
    attempt integer NOT NULL CHECK (attempt >= 0),
    evidence jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX transitions_deposit_idx ON transitions (deposit_id, created_at);

CREATE TRIGGER transitions_append_only
BEFORE UPDATE OR DELETE ON transitions
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

-- Refunds. The workflow columns (`requested_by`, `approved_by`, and the requested, approved,
-- sent, confirmed statuses) are the operator-approved flow that design PR 10 replaces.

CREATE TABLE refunds (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL,
    livemode boolean NOT NULL,
    deposit_id uuid NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    to_address text NOT NULL,
    tx_hash text,
    status text NOT NULL CHECK (status IN ('requested', 'approved', 'sent', 'confirmed')),
    requested_by text NOT NULL,
    approved_by text,
    created_at timestamptz NOT NULL DEFAULT now(),
    confirmation_evidence jsonb,
    next_check_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    confirmed_at timestamptz,
    tx_version bigint NOT NULL DEFAULT 0 CHECK (tx_version >= 0),
    route text NOT NULL,
    idempotency_key text
        CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255),
    CONSTRAINT refunds_deposit_fkey FOREIGN KEY (deposit_id, account_id, livemode)
        REFERENCES deposits (id, account_id, livemode),
    CONSTRAINT refunds_to_address_canonical_hex_check CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT refunds_tx_hash_canonical_hex_check
        CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT refunds_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$')
);

CREATE UNIQUE INDEX refunds_idempotency_unique
    ON refunds (deposit_id, to_address, amount_atomic);
CREATE UNIQUE INDEX refunds_idempotency_key_unique
    ON refunds (account_id, livemode, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
CREATE INDEX refunds_confirmation_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'sent';

COMMENT ON COLUMN refunds.confirmation_evidence IS
    'Most recent finalized transaction verification evidence, including mismatches.';
COMMENT ON COLUMN refunds.next_check_at IS
    'Earliest time the background confirmation worker should inspect a sent refund.';
COMMENT ON COLUMN refunds.route IS
    'Effective route selected when the refund was requested, including unsupported-asset fallback routing.';
COMMENT ON COLUMN refunds.idempotency_key IS
    'The Idempotency-Key the refund was requested with; a repeat with the same parameters returns this refund.';

-- Scoped through `refunds`.
CREATE TABLE refund_payment_claims (
    refund_id uuid NOT NULL REFERENCES refunds(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    claimed_amount_atomic numeric(78,0) NOT NULL CHECK (claimed_amount_atomic > 0),
    transferred_amount_atomic numeric(78,0) NOT NULL CHECK (transferred_amount_atomic > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (refund_id, chain_id, tx_hash, log_index),
    CONSTRAINT refund_payment_claims_log_unique UNIQUE (chain_id, tx_hash, log_index)
);

CREATE FUNCTION enforce_refund_payment_claim_capacity()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    claimed numeric(78,0);
BEGIN
    SELECT COALESCE(sum(claimed_amount_atomic), 0)
      INTO claimed
      FROM refund_payment_claims
     WHERE chain_id = NEW.chain_id
       AND tx_hash = NEW.tx_hash
       AND log_index = NEW.log_index;
    -- The global unique constraint owns reuse conflicts. Returning here lets
    -- INSERT ... ON CONFLICT report an unavailable claim without aborting the worker.
    IF claimed > 0 THEN
        RETURN NEW;
    END IF;
    IF claimed + NEW.claimed_amount_atomic > NEW.transferred_amount_atomic THEN
        RAISE EXCEPTION 'refund payment claim exceeds transfer log amount'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER refund_payment_claim_capacity
BEFORE INSERT ON refund_payment_claims
FOR EACH ROW EXECUTE FUNCTION enforce_refund_payment_claim_capacity();

COMMENT ON TABLE refund_payment_claims IS
    'Atomic allocation of finalized ERC-20 transfer logs to confirmed refunds.';

-- Webhooks (design D11): endpoints per account and mode, one event format, and a delivery per
-- event and endpoint.

CREATE TABLE webhook_endpoints (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    url text NOT NULL CHECK (url ~ '^https?://' AND char_length(url) <= 2048),
    enabled_events text[] NOT NULL DEFAULT '{*}',
    status text NOT NULL DEFAULT 'enabled' CHECK (status IN ('enabled', 'disabled')),
    disabled_reason text,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT webhook_endpoints_disabled_reason_check
        CHECK (status = 'disabled' OR disabled_reason IS NULL)
);

CREATE INDEX webhook_endpoints_scope_idx ON webhook_endpoints (account_id, livemode);

-- An event's `data`, `{"object": …}`, is the API representation of the object it is about,
-- rendered on the first delivery attempt ({} until then) and never re-rendered.
CREATE TABLE events (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    type text NOT NULL,
    object_type text NOT NULL CHECK (object_type IN ('deposit', 'quote')),
    object_id uuid NOT NULL,
    data jsonb NOT NULL DEFAULT '{}',
    created timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX events_object_idx ON events (object_type, object_id);
CREATE INDEX events_scope_created_idx ON events (account_id, livemode, created, id);

COMMENT ON COLUMN events.created IS
    'Stable event creation time used in the webhook envelope and age alerts.';

-- Scoped through `events`; the endpoint always belongs to the event's account and mode.
CREATE TABLE webhook_deliveries (
    event_id uuid NOT NULL REFERENCES events(id),
    endpoint_id uuid NOT NULL REFERENCES webhook_endpoints(id),
    next_attempt_at timestamptz NOT NULL,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    delivered_at timestamptz,
    response jsonb,
    PRIMARY KEY (event_id, endpoint_id)
);

CREATE INDEX webhook_deliveries_pending_idx
    ON webhook_deliveries (next_attempt_at, event_id, endpoint_id)
    WHERE delivered_at IS NULL;
CREATE INDEX webhook_deliveries_endpoint_idx ON webhook_deliveries (endpoint_id);

COMMENT ON COLUMN webhook_deliveries.attempts IS
    'Number of failed delivery attempts; successful delivery does not increment this counter.';

-- Stripe's idempotent requests (design §13), pruned after 24 hours.
CREATE TABLE idempotency_keys (
    account_id uuid NOT NULL REFERENCES accounts(id),
    livemode boolean NOT NULL,
    key text NOT NULL CHECK (octet_length(key) BETWEEN 1 AND 255),
    fingerprint bytea NOT NULL CHECK (octet_length(fingerprint) = 32),
    response jsonb,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (account_id, livemode, key)
);

-- Audit and reconciliation.

CREATE TABLE audit (
    id uuid PRIMARY KEY,
    account_id uuid REFERENCES accounts(id),
    actor_type text NOT NULL CHECK (actor_type IN ('user', 'api_key', 'admin', 'system')),
    actor_id text NOT NULL,
    action text NOT NULL,
    subject text NOT NULL,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX audit_account_idx ON audit (account_id, created_at) WHERE account_id IS NOT NULL;

COMMENT ON COLUMN audit.account_id IS
    'The account the action touched, for its security history; null for platform actions.';

CREATE TRIGGER audit_append_only
BEFORE UPDATE OR DELETE ON audit
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

CREATE TABLE reconciliation_findings (
    id uuid PRIMARY KEY,
    fingerprint text NOT NULL UNIQUE,
    check_name text NOT NULL,
    subjects jsonb NOT NULL,
    expected jsonb NOT NULL,
    observed jsonb NOT NULL,
    repair_applied boolean NOT NULL,
    incomplete boolean NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER reconciliation_findings_append_only
BEFORE UPDATE OR DELETE ON reconciliation_findings
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

CREATE TABLE reconciliation_blocks (
    block_key text PRIMARY KEY,
    scope text NOT NULL CHECK (scope IN ('address', 'chain')),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    address_id uuid REFERENCES addresses(id),
    check_name text NOT NULL,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT reconciliation_blocks_scope_address_check CHECK (
        (scope = 'address' AND address_id IS NOT NULL)
        OR (scope = 'chain' AND address_id IS NULL)
    )
);

CREATE TABLE reconciliation_deposit_cursors (
    chain_id bigint PRIMARY KEY CHECK (chain_id >= 0),
    next_block bigint NOT NULL CHECK (next_block >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE reconciliation_custody_cursors (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    factory text NOT NULL,
    token text NOT NULL,
    next_block bigint NOT NULL CHECK (next_block >= 0),
    flushed_event_total numeric(78,0) NOT NULL CHECK (flushed_event_total >= 0),
    treasury_inflow_total numeric(78,0) NOT NULL CHECK (treasury_inflow_total >= 0),
    updated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, factory, token)
);

-- Restore evidence. The RPO target is a code constant (`topup::heartbeat::RPO_SECONDS`).

CREATE TABLE heartbeat (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

GRANT USAGE, SELECT ON SEQUENCE heartbeat_id_seq TO topup_app;

COMMENT ON TABLE heartbeat IS
    'One row per minute; restore-check compares the restored row with an external failure point.';

-- Narrow the default operational grant. History and restore evidence are append-only for the
-- service. The reconciler only inserts blocks: an UPDATE could rewrite a block's scope or chain
-- and so lift a freeze, so only the admin lift deletes a block and only the database owner
-- changes one. The reconciler never deletes its scan cursors. The authorization table belongs to
-- the migrations.
REVOKE UPDATE, DELETE ON TABLE
    transitions,
    audit,
    reconciliation_findings,
    heartbeat
FROM topup_app;
REVOKE UPDATE ON TABLE reconciliation_blocks FROM topup_app;
REVOKE DELETE ON TABLE
    reconciliation_deposit_cursors,
    reconciliation_custody_cursors
FROM topup_app;
REVOKE INSERT, UPDATE, DELETE ON TABLE permissions FROM topup_app;
