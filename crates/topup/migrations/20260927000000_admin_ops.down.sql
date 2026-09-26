DROP INDEX outbox_deposit_id_idx;
REVOKE DELETE ON TABLE reconciliation_blocks FROM topup_app;
