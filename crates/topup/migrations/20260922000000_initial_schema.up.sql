-- Initial schema (docs/architecture.md §6). The service connects through a login role that is a
-- member of `topup_app`; migrations run as the trusted database owner.

DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_app') THEN
        CREATE ROLE topup_app NOLOGIN;
    END IF;
END;
$$;

GRANT USAGE ON SCHEMA public TO topup_app;

-- Every table below gets the operational grant; append-only and insert-only tables narrow it
-- after they are created. No application table grants TRUNCATE.
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

-- Identity and pause scopes.

CREATE TABLE products (
    id uuid PRIMARY KEY,
    slug text NOT NULL,
    webhook_url text NOT NULL,
    pubkey text NOT NULL,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT products_slug_unique UNIQUE (slug),
    CONSTRAINT products_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    )
);

CREATE TABLE accounts (
    id uuid PRIMARY KEY,
    product_id uuid NOT NULL REFERENCES products(id),
    external_id text NOT NULL,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    status text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'closed')),
    closed_at timestamptz,
    CONSTRAINT accounts_product_external_unique UNIQUE (product_id, external_id),
    CONSTRAINT accounts_paused_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    ),
    CONSTRAINT accounts_closed_at_status_check CHECK (
        status = 'closed' OR closed_at IS NULL
    )
);

COMMENT ON COLUMN accounts.closed_at IS
    'Time the workspace closed, for operators only. No service decision reads it: late funds are refundable because the product answers rejected, recorded as product_refused.';

CREATE TABLE route_pauses (
    route text PRIMARY KEY,
    paused_scopes text[] NOT NULL DEFAULT '{}',
    CONSTRAINT route_pauses_scopes_check CHECK (
        paused_scopes <@ ARRAY['quotes', 'addresses', 'settlement', 'flush', 'refunds']::text[]
    )
);

CREATE TABLE seen_signatures (
    kid text NOT NULL,
    signature_hash bytea NOT NULL,
    created timestamptz NOT NULL,
    PRIMARY KEY (kid, signature_hash)
);

CREATE INDEX seen_signatures_created_idx ON seen_signatures (created);

-- Addresses and the chain scanner.

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
    created_block bigint NOT NULL DEFAULT 0 CHECK (created_block >= 0),
    backfilled boolean NOT NULL DEFAULT false,
    requested_at timestamptz NOT NULL DEFAULT now(),
    CONSTRAINT addresses_chain_address_unique UNIQUE (chain_id, address),
    CONSTRAINT addresses_kind_fields_check CHECK (
        (kind = 'persistent' AND lock_ref IS NULL)
        OR (kind = 'lock' AND lock_ref IS NOT NULL)
    ),
    CONSTRAINT addresses_salt_canonical_hex_check CHECK (salt ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT addresses_address_canonical_hex_check CHECK (address ~ '^0x[0-9a-f]{40}$')
);

CREATE UNIQUE INDEX addresses_one_active_persistent_per_account_chain
    ON addresses (account_id, chain_id)
    WHERE kind = 'persistent' AND retired_at IS NULL;

CREATE UNIQUE INDEX addresses_account_lock_ref_unique
    ON addresses (account_id, lock_ref)
    WHERE kind = 'lock';

CREATE INDEX addresses_pending_backfill_idx
    ON addresses (chain_id, created_block, id)
    WHERE backfilled = false;

-- Bounds the persistent addresses the head scan watches when there are more than one log
-- request can carry.
CREATE INDEX addresses_persistent_requested_idx
    ON addresses (chain_id, requested_at)
    WHERE kind = 'persistent';

COMMENT ON COLUMN addresses.created_block IS
    'Earliest block the scanner must inspect for this counterfactual address; zero makes the first scanner pass check the full chain history.';
COMMENT ON COLUMN addresses.backfilled IS
    'True after the scanner transaction has covered created_block through the chain cursor.';

CREATE TABLE cursors (
    chain_id bigint PRIMARY KEY CHECK (chain_id >= 0),
    scanned_block bigint NOT NULL CHECK (scanned_block >= 0),
    scanned_block_time timestamptz
);

COMMENT ON COLUMN cursors.scanned_block_time IS
    'Block time of the finalized head when the scanner last committed through it; a lower bound on the time of scanned_block. Rate locks expire only once this passes expires_at.';

-- Display-only transfers seen above the finalized head (architecture §8, §12). Rows never feed
-- deposits, transitions, rate locks, exposure, settlement, or reconciliation; the finalized
-- scanner deletes them in the same transaction that advances its cursor past their block.
CREATE TABLE pending_transfers (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
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

-- Flushes.

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
    ),
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
    WHERE state NOT IN ('swept', 'rejected');

-- Rate-lock expiry waits while a payment mined inside the window is still unconfirmed.
CREATE INDEX deposits_detected_address_idx ON deposits (address_id) WHERE state = 'detected';

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

-- Rate locks (architecture §9).

CREATE TABLE rate_locks (
    address_id uuid PRIMARY KEY REFERENCES addresses(id),
    route text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    price_scaled numeric(78,0) NOT NULL CHECK (price_scaled >= 0),
    expires_at timestamptz NOT NULL,
    consumed_by uuid UNIQUE REFERENCES deposits(id),
    credit_minor numeric(78,0) NOT NULL CHECK (credit_minor >= 0),
    status text NOT NULL DEFAULT 'open',
    exposure_reserved boolean NOT NULL DEFAULT false,
    created_at timestamptz NOT NULL DEFAULT now(),
    closed_at timestamptz,
    CONSTRAINT rate_locks_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$'),
    CONSTRAINT rate_locks_price_scaled_integer_check
        CHECK (price_scaled = trunc(price_scaled) AND price_scaled::text ~ '^[0-9]+$'),
    CONSTRAINT rate_locks_status_check
        CHECK (status IN ('open', 'consumed', 'expired', 'cancelled')),
    CONSTRAINT rate_locks_status_consumption_check CHECK (
        (status = 'consumed' AND consumed_by IS NOT NULL)
        OR (status <> 'consumed' AND consumed_by IS NULL)
    ),
    CONSTRAINT rate_locks_closed_at_check CHECK (
        (status = 'open' AND closed_at IS NULL)
        OR (status <> 'open' AND closed_at IS NOT NULL)
    )
);

CREATE INDEX rate_locks_open_expiry_idx
    ON rate_locks (expires_at, address_id)
    WHERE status = 'open';

CREATE TABLE lock_exposure (
    scope_key text PRIMARY KEY,
    open_minor numeric(78,0) NOT NULL CHECK (open_minor >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

-- Settlement and product events.

CREATE TABLE settlements (
    deposit_id uuid PRIMARY KEY REFERENCES deposits(id),
    product_id uuid NOT NULL REFERENCES products(id),
    key text NOT NULL,
    payload jsonb NOT NULL,
    status text NOT NULL CHECK (status IN ('intent', 'sent', 'accepted', 'rejected')),
    destination_tx_id text,
    receipt jsonb,
    sent_at timestamptz,
    resend_forbidden boolean NOT NULL DEFAULT false
);

CREATE UNIQUE INDEX settlements_product_destination_unique
    ON settlements (product_id, destination_tx_id)
    WHERE destination_tx_id IS NOT NULL;

CREATE TABLE outbox (
    id uuid PRIMARY KEY,
    event_type text NOT NULL,
    payload jsonb NOT NULL,
    next_attempt_at timestamptz NOT NULL,
    delivered_at timestamptz,
    response jsonb,
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX outbox_pending_idx
    ON outbox (next_attempt_at, id)
    WHERE delivered_at IS NULL;

COMMENT ON COLUMN outbox.attempts IS
    'Number of failed delivery attempts; successful delivery does not increment this counter.';
COMMENT ON COLUMN outbox.created_at IS
    'Stable event creation time used in the webhook envelope and age alerts.';

-- Refunds.

CREATE TABLE refunds (
    id uuid PRIMARY KEY,
    deposit_id uuid NOT NULL REFERENCES deposits(id),
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
    CONSTRAINT refunds_to_address_canonical_hex_check CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    CONSTRAINT refunds_tx_hash_canonical_hex_check
        CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    CONSTRAINT refunds_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$')
);

CREATE UNIQUE INDEX refunds_idempotency_unique
    ON refunds (deposit_id, to_address, amount_atomic);

CREATE INDEX refunds_confirmation_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'sent';

COMMENT ON COLUMN refunds.confirmation_evidence IS
    'Most recent finalized transaction verification evidence, including mismatches.';
COMMENT ON COLUMN refunds.next_check_at IS
    'Earliest time the background confirmation worker should inspect a sent refund.';
COMMENT ON COLUMN refunds.route IS
    'Effective route selected when the refund was requested, including unsupported-asset fallback routing.';

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

-- Audit and reconciliation.

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
-- and so lift a freeze, so only the database owner changes or deletes block rows. The
-- reconciler never deletes its scan cursors.
REVOKE UPDATE, DELETE ON TABLE
    transitions,
    audit,
    reconciliation_findings,
    reconciliation_blocks,
    heartbeat
FROM topup_app;
REVOKE DELETE ON TABLE
    reconciliation_deposit_cursors,
    reconciliation_custody_cursors
FROM topup_app;
