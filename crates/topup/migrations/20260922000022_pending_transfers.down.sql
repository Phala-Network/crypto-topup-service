DROP INDEX addresses_persistent_requested_idx;
ALTER TABLE addresses DROP COLUMN requested_at;
DROP TABLE pending_transfers;
