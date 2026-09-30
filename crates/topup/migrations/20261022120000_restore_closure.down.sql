-- Restores the schema before 20261022120000_restore_closure. A deposit restored from a delivered
-- event stays as it is; a successor recorded after this runs no longer names it.

DROP TABLE restore_deposit_tombstones;
