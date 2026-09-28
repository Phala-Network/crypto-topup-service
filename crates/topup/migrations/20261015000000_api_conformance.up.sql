-- API conformance with Stripe's conventions (docs/design/multi-tenant.md, "API conformance"
-- amendment).
--
-- Events are snapshots: `data.object` is rendered in the transaction that changes the object and
-- never changes afterwards (https://docs.stripe.com/api/events/object), so the service may no
-- longer update or delete an event. Rows written before this migration may hold `{}`; the check
-- applies to every new row.
ALTER TABLE events
    ADD COLUMN request_id text CHECK (request_id ~ '^req_[0-9a-f]{32}$'),
    ADD COLUMN idempotency_key text CHECK (char_length(idempotency_key) BETWEEN 1 AND 255),
    ADD CONSTRAINT events_idempotency_key_request_check
        CHECK (idempotency_key IS NULL OR request_id IS NOT NULL),
    ADD CONSTRAINT events_data_object_check CHECK (data ? 'object') NOT VALID;
REVOKE UPDATE, DELETE ON TABLE events FROM topup_app;

COMMENT ON COLUMN events.data IS
    'Stripe''s event data: `object`, the object''s API representation when the event was created, and `previous_attributes` on `*.updated` events. Written once, with the change.';
COMMENT ON COLUMN events.request_id IS
    'The Request-Id of the API request that caused the event; NULL for the service''s own workers.';
COMMENT ON COLUMN events.idempotency_key IS
    'The Idempotency-Key of that request, when it sent one.';

-- Treasury events are named after their object, a top-level resource (`/v1/treasuries`).
UPDATE events SET type = 'treasury.created' WHERE type = 'account.treasury.pending';
UPDATE events SET type = 'treasury.updated' WHERE type = 'account.treasury.updated';
UPDATE events SET type = 'treasury.canceled' WHERE type = 'account.treasury.canceled';
UPDATE webhook_endpoints
SET enabled_events = ARRAY(
    SELECT DISTINCT CASE event_type
        WHEN 'account.treasury.pending' THEN 'treasury.created'
        WHEN 'account.treasury.updated' THEN 'treasury.updated'
        WHEN 'account.treasury.canceled' THEN 'treasury.canceled'
        ELSE event_type
    END
    FROM unnest(enabled_events) AS event_type
    ORDER BY 1
)
WHERE enabled_events && ARRAY[
    'account.treasury.pending', 'account.treasury.updated', 'account.treasury.canceled'
]::text[];

-- Delivery health, shown on the endpoint object: the latest attempt to deliver to it.
ALTER TABLE webhook_endpoints
    ADD COLUMN last_attempt_at timestamptz,
    ADD COLUMN last_attempt_status integer CHECK (last_attempt_status BETWEEN 100 AND 599);
COMMENT ON COLUMN webhook_endpoints.last_attempt_at IS
    'When the service last attempted a delivery to the endpoint.';
COMMENT ON COLUMN webhook_endpoints.last_attempt_status IS
    'The HTTP status of that attempt; NULL when no response arrived (timeout, connection failure).';

-- `GET /v1/events?delivery_success=false` finds events with an undelivered delivery.
CREATE INDEX webhook_deliveries_undelivered_idx
    ON webhook_deliveries (event_id) WHERE delivered_at IS NULL;
