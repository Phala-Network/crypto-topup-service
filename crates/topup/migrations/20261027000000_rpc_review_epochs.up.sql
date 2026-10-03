-- Preserve old branch evidence without comparing it with a recovered chain.
ALTER TABLE rpc_window_reviews ADD COLUMN epoch bigint NOT NULL DEFAULT 0;
ALTER TABLE rpc_window_reviews ADD COLUMN replayed_at timestamptz;
ALTER TABLE rpc_window_reviews DROP CONSTRAINT rpc_window_reviews_chain_id_group_id_request_digest_key;
ALTER TABLE rpc_window_reviews ADD CONSTRAINT rpc_window_reviews_identity_epoch UNIQUE (chain_id, group_id, request_digest, epoch);
GRANT UPDATE (replayed_at) ON rpc_window_reviews TO topup_app;
CREATE TABLE rpc_role_bindings (
    chain_id bigint NOT NULL,
    role text NOT NULL CHECK (role IN ('a', 'b')),
    group_id text NOT NULL,
    PRIMARY KEY (chain_id, role),
    UNIQUE (chain_id, group_id)
);
GRANT SELECT, INSERT ON rpc_role_bindings TO topup_app;
REVOKE UPDATE, DELETE ON rpc_role_bindings FROM topup_app;
CREATE TABLE rpc_reorg_ranges (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    chain_id bigint NOT NULL,
    group_id text NOT NULL,
    epoch bigint NOT NULL,
    from_block bigint NOT NULL,
    to_block bigint NOT NULL,
    replayed_through bigint,
    created_at timestamptz NOT NULL DEFAULT now()
);
GRANT SELECT, INSERT ON rpc_reorg_ranges TO topup_app;
REVOKE UPDATE, DELETE ON rpc_reorg_ranges FROM topup_app;
GRANT UPDATE (replayed_through) ON rpc_reorg_ranges TO topup_app;
