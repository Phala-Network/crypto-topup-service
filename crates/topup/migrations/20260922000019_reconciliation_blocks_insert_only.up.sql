-- The reconciler only inserts blocks; an UPDATE could rewrite a block's scope or chain and so
-- lift a freeze. Only the database owner changes or deletes block rows.
REVOKE UPDATE ON TABLE reconciliation_blocks FROM topup_app;
