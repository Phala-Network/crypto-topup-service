DROP TABLE rpc_reorg_ranges;
DROP TABLE rpc_role_bindings;
ALTER TABLE rpc_window_reviews DROP CONSTRAINT rpc_window_reviews_identity_epoch;
ALTER TABLE rpc_window_reviews DROP COLUMN epoch, DROP COLUMN replayed_at;
ALTER TABLE rpc_window_reviews ADD UNIQUE (chain_id, group_id, request_digest);
