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

GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE flush_exclusions TO topup_app;
