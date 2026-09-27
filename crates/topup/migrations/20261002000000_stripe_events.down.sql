-- Restores the schema only; format-2 rows keep their rendered payloads.
COMMENT ON COLUMN outbox.payload IS NULL;
DROP INDEX outbox_object_idx;
ALTER TABLE outbox
    DROP CONSTRAINT outbox_format_object_check,
    DROP COLUMN object_id,
    DROP COLUMN object_type,
    DROP COLUMN product_id,
    DROP COLUMN format;
