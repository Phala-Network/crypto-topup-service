-- Restores the schema before 20261021120000_deposit_revisions. It refuses to run while a deposit
-- recorded after a reversed one at the same receipt position exists.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM deposits WHERE revision > 0) THEN
        RAISE EXCEPTION 'deposits with a revision above 0 exist';
    END IF;
END
$$;

DROP INDEX deposits_chain_event_live_unique;
ALTER TABLE deposits DROP CONSTRAINT deposits_chain_event_unique;
ALTER TABLE deposits
    ADD CONSTRAINT deposits_chain_event_unique UNIQUE (chain_id, tx_hash, receipt_log_index);
ALTER TABLE deposits DROP COLUMN revision;
