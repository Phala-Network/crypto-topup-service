-- The admin-signed `POST /v1/admin/reconciliation-blocks/{block_key}/lift` deletes a block row and
-- writes its audit row in the same transaction. Blocks stay closed to UPDATE: an update could
-- rewrite a block's scope or chain.
GRANT DELETE ON TABLE reconciliation_blocks TO topup_app;

-- The signed support lookup lists each deposit's webhook events.
CREATE INDEX outbox_deposit_id_idx ON outbox ((payload ->> 'deposit_id'));
