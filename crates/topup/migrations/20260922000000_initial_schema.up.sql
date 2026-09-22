CREATE FUNCTION reject_append_only_mutation()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    RAISE EXCEPTION '% is append-only', TG_TABLE_NAME USING ERRCODE = '55000';
END;
$$;

CREATE TABLE products (
    id uuid PRIMARY KEY,
    slug text NOT NULL,
    settlement_url text NOT NULL,
    webhook_url text NOT NULL,
    pubkey text NOT NULL,
    kid text NOT NULL,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT products_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    )
);

CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    product_id uuid NOT NULL REFERENCES products(id),
    external_id text NOT NULL,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT accounts_product_external_unique UNIQUE (product_id, external_id),
    CONSTRAINT accounts_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    )
);

CREATE TABLE addresses (
    id uuid PRIMARY KEY,
    account_id uuid NOT NULL REFERENCES accounts(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    kind text NOT NULL CHECK (kind IN ('persistent', 'lock')),
    version bigint NOT NULL CHECK (version >= 0),
    lock_ref text,
    salt text NOT NULL,
    address text NOT NULL,
    retired_at timestamptz,
    CONSTRAINT addresses_chain_address_unique UNIQUE (chain_id, address),
    CONSTRAINT addresses_kind_fields_check CHECK (
        (kind = 'persistent' AND lock_ref IS NULL)
        OR (kind = 'lock' AND lock_ref IS NOT NULL)
    )
);

CREATE UNIQUE INDEX addresses_one_active_persistent_per_account_chain
    ON addresses (account_id, chain_id)
    WHERE kind = 'persistent' AND retired_at IS NULL;

CREATE TABLE cursors (
    chain_id bigint PRIMARY KEY CHECK (chain_id >= 0),
    scanned_block bigint NOT NULL CHECK (scanned_block >= 0)
);

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
    CONSTRAINT flushes_operator_nonce_unique UNIQUE (chain_id, operator, nonce)
);

CREATE TABLE deposits (
    id uuid PRIMARY KEY,
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    block_time timestamptz NOT NULL,
    address_id uuid NOT NULL REFERENCES addresses(id),
    account_id uuid NOT NULL REFERENCES accounts(id),
    route text,
    route_version bigint CHECK (route_version >= 0),
    asset_contract text NOT NULL,
    from_address text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    state text NOT NULL CHECK (state IN ('detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected')),
    reason text CHECK (reason IN (
        'unsupported_asset', 'below_minimum', 'out_of_range', 'sanctioned', 'out_of_bounds',
        'product_refused'
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
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT deposits_chain_event_unique UNIQUE (chain_id, tx_hash, log_index),
    CONSTRAINT deposits_reason_state_check CHECK (
        (state = 'rejected' AND reason IS NOT NULL)
        OR (state <> 'rejected' AND reason IS NULL)
    ),
    CONSTRAINT deposits_lease_check CHECK (
        (lease_token IS NULL AND lease_until IS NULL)
        OR (lease_token IS NOT NULL AND lease_until IS NOT NULL)
    )
);

CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state NOT IN ('swept', 'rejected');

CREATE TABLE rate_locks (
    address_id uuid PRIMARY KEY REFERENCES addresses(id),
    route text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    price_scaled numeric(78,0) NOT NULL CHECK (price_scaled >= 0),
    expires_at timestamptz NOT NULL,
    consumed_by uuid UNIQUE REFERENCES deposits(id)
);

CREATE TABLE transitions (
    id uuid PRIMARY KEY,
    deposit_id uuid NOT NULL REFERENCES deposits(id),
    from_state text NOT NULL CHECK (from_state IN ('detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected')),
    to_state text NOT NULL CHECK (to_state IN ('detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected')),
    attempt integer NOT NULL CHECK (attempt >= 0),
    evidence jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER transitions_append_only
BEFORE UPDATE OR DELETE ON transitions
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

CREATE TABLE settlements (
    deposit_id uuid PRIMARY KEY REFERENCES deposits(id),
    product_id uuid NOT NULL REFERENCES products(id),
    key text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('intent', 'sent', 'accepted', 'rejected')),
    destination_tx_id text,
    receipt jsonb,
    sent_at timestamptz
);

CREATE UNIQUE INDEX settlements_product_destination_unique
    ON settlements (product_id, destination_tx_id)
    WHERE destination_tx_id IS NOT NULL;

CREATE TABLE flushed (
    flush_id uuid NOT NULL REFERENCES flushes(id),
    address_id uuid NOT NULL REFERENCES addresses(id),
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    log_index bigint NOT NULL CHECK (log_index >= 0),
    PRIMARY KEY (flush_id, address_id)
);

CREATE TABLE refunds (
    id uuid PRIMARY KEY,
    deposit_id uuid NOT NULL REFERENCES deposits(id),
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    to_address text NOT NULL,
    tx_hash text,
    status text NOT NULL CHECK (status IN ('requested', 'approved', 'sent', 'confirmed')),
    requested_by text NOT NULL,
    approved_by text,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE outbox (
    id uuid PRIMARY KEY,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    next_attempt_at timestamptz NOT NULL,
    delivered_at timestamptz,
    response jsonb
);

CREATE INDEX outbox_pending_idx
    ON outbox (next_attempt_at, id)
    WHERE delivered_at IS NULL;

CREATE TABLE audit (
    id uuid PRIMARY KEY,
    actor text NOT NULL,
    action text NOT NULL,
    subject text NOT NULL,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TRIGGER audit_append_only
BEFORE UPDATE OR DELETE ON audit
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

COMMENT ON FUNCTION reject_append_only_mutation() IS
    'Trigger enforcement protects append-only history for every database role, including table owners.';
