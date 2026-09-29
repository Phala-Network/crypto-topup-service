-- Restores the schema before 20261021160000_restore_correctness. Run it only while no re-issued
-- quote is open: without `quotes.restore_id` a re-issued quote's lock would apply.

ALTER TABLE quotes DROP COLUMN restore_id;
DROP TABLE restore_delivered_credits;
