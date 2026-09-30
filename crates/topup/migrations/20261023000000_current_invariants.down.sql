-- Restores the schema before 20261023000000_current_invariants.

ALTER TABLE events DROP CONSTRAINT events_deposit_identity_check;
ALTER TABLE events DROP CONSTRAINT events_data_object_check;
ALTER TABLE events ADD CONSTRAINT events_data_object_check CHECK (data ? 'object') NOT VALID;
ALTER TABLE events ALTER COLUMN data SET DEFAULT '{}';
ALTER TABLE deposits DROP CONSTRAINT deposits_tx_origin_check;
