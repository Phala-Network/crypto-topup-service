-- Chain-sourced sweeps (docs/design/multi-tenant.md §4, §13, §14, §16 PR 4). The service sends no
-- transactions: anyone, usually the merchant, calls the permissionless factory's `flush`, and the
-- finalized scanner indexes the factory's `ForwarderCreated`, `Flushed`, and `FlushFailed` events
-- for known `(address, treasury)` pairs. The operator flusher's plans, nonces, and exclusions go,
-- with the `flush` pause scope and the treasury-inflow totals of the old custody check, which
-- reconciliation per forwarder replaces.

ALTER TABLE deposits DROP COLUMN flush_id;
DROP TABLE flush_exclusions;
DROP TABLE flushed;
DROP TABLE flushes;
DROP TABLE reconciliation_custody_cursors;

UPDATE accounts SET paused_scopes = array_remove(paused_scopes, 'flush');
UPDATE customers SET paused_scopes = array_remove(paused_scopes, 'flush');
UPDATE route_pauses SET paused_scopes = array_remove(paused_scopes, 'flush');
ALTER TABLE accounts DROP CONSTRAINT accounts_paused_scopes_check;
ALTER TABLE accounts ADD CONSTRAINT accounts_paused_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'refunds']::text[]
);
ALTER TABLE customers DROP CONSTRAINT customers_paused_scopes_check;
ALTER TABLE customers ADD CONSTRAINT customers_paused_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'refunds']::text[]
);
ALTER TABLE route_pauses DROP CONSTRAINT route_pauses_scopes_check;
ALTER TABLE route_pauses ADD CONSTRAINT route_pauses_scopes_check CHECK (
    paused_scopes <@ ARRAY['quotes', 'settlement', 'refunds']::text[]
);
COMMENT ON COLUMN accounts.paused_scopes IS
    'Operator and self-serve pauses of the whole account (design §12).';

ALTER TABLE addresses ADD COLUMN deployed_block bigint CHECK (deployed_block >= 0);
COMMENT ON COLUMN addresses.deployed_block IS
    'Block of the finalized ForwarderCreated event for this address and treasury; null while the forwarder is counterfactual.';

-- Scoped through `addresses`.
CREATE TABLE flushed (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    address_id uuid NOT NULL REFERENCES addresses(id),
    token text NOT NULL,
    treasury text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, tx_hash, log_index),
    CONSTRAINT flushed_tx_hash_canonical_hex_check CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT flushed_block_hash_canonical_hex_check CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT flushed_token_canonical_hex_check CHECK (token ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT flushed_treasury_canonical_hex_check CHECK (treasury ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT flushed_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$')
);

CREATE INDEX flushed_address_idx ON flushed (address_id, token, block_number, log_index);

COMMENT ON TABLE flushed IS
    'Finalized ForwarderFactory Flushed events, whoever sent them, for a known address and its own treasury.';

-- Scoped through `addresses`.
CREATE TABLE flush_failures (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    address_id uuid NOT NULL REFERENCES addresses(id),
    token text NOT NULL,
    reason text NOT NULL,
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, tx_hash, log_index),
    CONSTRAINT flush_failures_tx_hash_canonical_hex_check CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT flush_failures_block_hash_canonical_hex_check
        CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT flush_failures_token_canonical_hex_check CHECK (token ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT flush_failures_reason_hex_check
        CHECK (reason ~ '^0x([0-9a-f]{2})*$' AND length(reason) <= 2 + 2 * 256)
);

CREATE INDEX flush_failures_address_idx ON flush_failures (address_id, block_number);

COMMENT ON TABLE flush_failures IS
    'Finalized ForwarderFactory FlushFailed events for a known address: the target was skipped and its deposits stay unswept.';
COMMENT ON COLUMN flush_failures.reason IS
    'Revert data as 0x-prefixed hex, truncated by the factory to 256 bytes.';

-- Deposits of an address are marked swept when a finalized `Flushed` event is indexed for it.
CREATE INDEX deposits_credited_address_idx ON deposits (address_id) WHERE state = 'credited';

-- Finalized chain facts are never rewritten by the service.
REVOKE UPDATE, DELETE ON TABLE flushed, flush_failures FROM topup_app;
