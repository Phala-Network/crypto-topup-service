-- Restores the schema before 20261013000000_webhook_endpoints (keeping `treasury`, design PR 7's type). Events about webhook endpoints and
-- deleted endpoints have no equivalent and are discarded with their deliveries.

DROP INDEX events_scope_type_created_idx;

DELETE FROM webhook_deliveries
WHERE event_id IN (SELECT id FROM events WHERE object_type = 'webhook_endpoint')
   OR endpoint_id IN (SELECT id FROM webhook_endpoints WHERE deleted_at IS NOT NULL);
DELETE FROM events WHERE object_type = 'webhook_endpoint';
DELETE FROM webhook_endpoints WHERE deleted_at IS NOT NULL;
ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote', 'api_key', 'account', 'refund', 'treasury'));

DROP INDEX webhook_deliveries_pending_idx;
DROP INDEX webhook_deliveries_endpoint_idx;
ALTER TABLE webhook_deliveries
    DROP CONSTRAINT webhook_deliveries_outcome_check,
    DROP COLUMN url,
    DROP COLUMN failed_at;
CREATE INDEX webhook_deliveries_pending_idx
    ON webhook_deliveries (next_attempt_at, event_id, endpoint_id)
    WHERE delivered_at IS NULL;
CREATE INDEX webhook_deliveries_endpoint_idx ON webhook_deliveries (endpoint_id);

ALTER TABLE webhook_endpoints
    DROP CONSTRAINT webhook_endpoints_enabled_events_check,
    DROP CONSTRAINT webhook_endpoints_disabled_reason_value_check,
    DROP COLUMN deleted_at,
    DROP COLUMN metadata,
    DROP COLUMN description;
