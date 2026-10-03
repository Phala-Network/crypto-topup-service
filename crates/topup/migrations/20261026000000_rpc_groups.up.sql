-- Durable group identity, monotonic heads, historical review and audited recovery.
CREATE TABLE rpc_config_acceptances (
    config_digest text PRIMARY KEY,
    accepted_at timestamptz NOT NULL DEFAULT now()
);
CREATE TABLE rpc_member_validations (
    config_digest text NOT NULL REFERENCES rpc_config_acceptances,
    group_id text NOT NULL,
    member_id text NOT NULL,
    chain_id bigint NOT NULL CHECK (chain_id > 0),
    genesis_hash text NOT NULL,
    validated_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (config_digest, group_id, member_id)
);
CREATE TABLE rpc_chain_state (
    chain_id bigint PRIMARY KEY,
    frozen boolean NOT NULL DEFAULT false,
    awaiting_anchor boolean NOT NULL DEFAULT false,
    recovery_pending boolean NOT NULL DEFAULT false,
    reason text,
    epoch bigint NOT NULL DEFAULT 0 CHECK (epoch >= 0)
);
CREATE TABLE rpc_watermarks (
    chain_id bigint NOT NULL,
    group_id text NOT NULL,
    tag text NOT NULL CHECK (tag IN ('latest', 'safe', 'finalized', 'cursor')),
    epoch bigint NOT NULL DEFAULT 0,
    number bigint NOT NULL CHECK (number >= 0),
    hash text NOT NULL,
    parent_hash text NOT NULL,
    member_id text NOT NULL,
    config_digest text NOT NULL,
    accepted_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, group_id, tag, epoch)
);
CREATE TABLE rpc_window_reviews (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    chain_id bigint NOT NULL,
    group_id text NOT NULL,
    from_block bigint NOT NULL CHECK (from_block >= 0),
    to_block bigint NOT NULL CHECK (to_block >= from_block),
    request jsonb NOT NULL,
    request_digest text NOT NULL,
    answering_member text NOT NULL,
    end_hash text NOT NULL,
    reviewed_by text,
    reviewed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (chain_id, group_id, request_digest)
);
CREATE INDEX rpc_window_reviews_pending ON rpc_window_reviews(chain_id, from_block) WHERE reviewed_at IS NULL;
CREATE TABLE rpc_recoveries (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    chain_id bigint NOT NULL,
    epoch bigint NOT NULL,
    evidence jsonb NOT NULL,
    actor text NOT NULL,
    reason text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
GRANT SELECT, INSERT, UPDATE ON rpc_config_acceptances, rpc_member_validations, rpc_chain_state, rpc_watermarks, rpc_window_reviews TO topup_app;
GRANT SELECT ON rpc_recoveries TO topup_app;
REVOKE INSERT ON rpc_recoveries FROM topup_app;
-- The 0.4 tenancy migration grants default CRUD: revoke destructive/immutable writes explicitly.
REVOKE DELETE ON rpc_config_acceptances, rpc_member_validations, rpc_chain_state, rpc_watermarks, rpc_window_reviews, rpc_recoveries FROM topup_app;
REVOKE UPDATE ON rpc_config_acceptances, rpc_member_validations, rpc_chain_state, rpc_window_reviews, rpc_recoveries FROM topup_app;
GRANT UPDATE (validated_at) ON rpc_member_validations TO topup_app;
GRANT UPDATE (frozen, reason, awaiting_anchor) ON rpc_chain_state TO topup_app;
GRANT UPDATE (reviewed_at, reviewed_by) ON rpc_window_reviews TO topup_app;
