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

REVOKE ALL PRIVILEGES ON TABLE
    reconciliation_findings,
    reconciliation_blocks,
    reconciliation_deposit_cursors,
    reconciliation_custody_cursors
FROM topup_app;
GRANT SELECT, INSERT ON TABLE reconciliation_findings TO topup_app;
GRANT SELECT, INSERT, UPDATE ON TABLE
    reconciliation_blocks,
    reconciliation_deposit_cursors,
    reconciliation_custody_cursors
TO topup_app;
