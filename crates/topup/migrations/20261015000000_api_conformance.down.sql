-- Restores the schema before 20261015000000_api_conformance. Events keep their rendered data.

DROP INDEX webhook_deliveries_undelivered_idx;
ALTER TABLE webhook_endpoints DROP COLUMN last_attempt_status, DROP COLUMN last_attempt_at;

UPDATE webhook_endpoints
SET enabled_events = ARRAY(
    SELECT DISTINCT CASE event_type
        WHEN 'treasury.created' THEN 'account.treasury.pending'
        WHEN 'treasury.updated' THEN 'account.treasury.updated'
        WHEN 'treasury.canceled' THEN 'account.treasury.canceled'
        ELSE event_type
    END
    FROM unnest(enabled_events) AS event_type
    ORDER BY 1
)
WHERE enabled_events && ARRAY['treasury.created', 'treasury.updated', 'treasury.canceled']::text[];
UPDATE events SET type = 'account.treasury.pending' WHERE type = 'treasury.created';
UPDATE events SET type = 'account.treasury.updated' WHERE type = 'treasury.updated';
UPDATE events SET type = 'account.treasury.canceled' WHERE type = 'treasury.canceled';

GRANT UPDATE, DELETE ON TABLE events TO topup_app;
ALTER TABLE events
    DROP CONSTRAINT events_data_object_check,
    DROP CONSTRAINT events_idempotency_key_request_check,
    DROP COLUMN idempotency_key,
    DROP COLUMN request_id;
COMMENT ON COLUMN events.data IS NULL;
