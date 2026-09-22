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

GRANT SELECT, INSERT ON TABLE reconciliation_findings TO topup_app;
GRANT SELECT, INSERT, UPDATE ON TABLE reconciliation_blocks TO topup_app;
REVOKE TRUNCATE ON TABLE reconciliation_findings, reconciliation_blocks FROM topup_app;
