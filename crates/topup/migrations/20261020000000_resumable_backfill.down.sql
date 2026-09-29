-- Restores the schema before 20261020000000_resumable_backfill; an unfinished backfill restarts
-- from its creation block.
ALTER TABLE addresses DROP COLUMN backfilled_through;
