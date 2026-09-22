DROP INDEX addresses_pending_backfill_idx;

ALTER TABLE addresses
    DROP COLUMN backfilled,
    DROP COLUMN created_block;
