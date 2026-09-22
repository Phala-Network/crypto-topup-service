CREATE TABLE seen_signatures (
    kid text NOT NULL,
    signature_hash bytea NOT NULL,
    created timestamptz NOT NULL,
    PRIMARY KEY (kid, signature_hash)
);

CREATE INDEX seen_signatures_created_idx ON seen_signatures (created);

GRANT SELECT, INSERT, DELETE ON TABLE seen_signatures TO topup_app;
REVOKE TRUNCATE ON TABLE seen_signatures FROM topup_app;
