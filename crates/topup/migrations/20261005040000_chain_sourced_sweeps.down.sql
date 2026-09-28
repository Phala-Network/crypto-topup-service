-- Restores the operator flusher's tables and the treasury-inflow totals exactly as
-- 20261004000000_multi_tenant created them. Chain-sourced sweep records are discarded.

DROP INDEX deposits_credited_address_idx;
DROP TABLE flush_failures;
DROP TABLE flushed;
ALTER TABLE addresses DROP COLUMN deployed_block;

ALTER TABLE accounts DROP CONSTRAINT accounts_paused_scopes_check;
ALTER TABLE accounts ADD CONSTRAINT accounts_paused_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
);
ALTER TABLE customers DROP CONSTRAINT customers_paused_scopes_check;
ALTER TABLE customers ADD CONSTRAINT customers_paused_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
);
ALTER TABLE route_pauses DROP CONSTRAINT route_pauses_scopes_check;
ALTER TABLE route_pauses ADD CONSTRAINT route_pauses_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'flush', 'refunds']::text[]
);
COMMENT ON COLUMN accounts.paused_scopes IS
    'Operator and self-serve pauses of the whole account (design §12); flush is removed with the flusher.';

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


ALTER TABLE deposits ADD COLUMN flush_id uuid REFERENCES flushes(id);

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

REVOKE DELETE ON TABLE reconciliation_custody_cursors FROM topup_app;
