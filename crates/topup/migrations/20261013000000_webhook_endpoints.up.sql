-- Design PR 8: webhook endpoints through the API, account events, and delivery
-- (docs/design/multi-tenant.md D11, §11, §13).
--
-- Merchants manage their endpoints with `/v1/webhook_endpoints` (at most 16 per account and mode,
-- counted by the service under the account's row lock); the operator no longer registers a URL.
-- A deleted endpoint is kept, with `deleted_at`, until its deliveries are gone: it receives the
-- event about its own deletion and nothing else. Deliveries are retried until delivered, never
-- given up on a timer (owner decision of 2026-09-28): only the receiver's `410 Gone` or the
-- merchant disabling or deleting the endpoint stops them.
--
-- Applied after design PR 7's 20261012000000_treasuries: the object type constraint keeps its
-- `treasury` and adds `webhook_endpoint`.

ALTER TABLE webhook_endpoints
    ADD COLUMN description text CHECK (char_length(description) BETWEEN 1 AND 5000),
    ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}'
        CONSTRAINT webhook_endpoints_metadata_check CHECK (metadata_is_valid(metadata)),
    ADD COLUMN deleted_at timestamptz,
    ADD CONSTRAINT webhook_endpoints_disabled_reason_value_check
        CHECK (disabled_reason = 'gone'),
    ADD CONSTRAINT webhook_endpoints_enabled_events_check
        CHECK (cardinality(enabled_events) BETWEEN 1 AND 100);

COMMENT ON COLUMN webhook_endpoints.disabled_reason IS
    'Why the service disabled the endpoint: `gone` (it answered 410); NULL when the merchant disabled it. Failing deliveries never disable an endpoint.';
COMMENT ON COLUMN webhook_endpoints.deleted_at IS
    'When the merchant deleted the endpoint; it is then invisible to the API and receives only the notice of its deletion.';

-- A delivery is retried until delivered; it stops, with `failed_at`, only on a `410 Gone` or when
-- its endpoint is disabled or deleted (a resend restarts it).
-- `url` is set on an endpoint's notice of its own change or deletion, or its test event: that
-- delivery goes to the URL the endpoint had before the change, whatever its status now, so a
-- change is announced where the merchant was listening.
ALTER TABLE webhook_deliveries
    ADD COLUMN failed_at timestamptz,
    ADD COLUMN url text CHECK (url ~ '^https?://' AND char_length(url) <= 2048),
    ADD CONSTRAINT webhook_deliveries_outcome_check
        CHECK (delivered_at IS NULL OR failed_at IS NULL);

DROP INDEX webhook_deliveries_pending_idx;
DROP INDEX webhook_deliveries_endpoint_idx;
CREATE INDEX webhook_deliveries_endpoint_idx ON webhook_deliveries (endpoint_id, event_id);
CREATE INDEX webhook_deliveries_pending_idx
    ON webhook_deliveries (endpoint_id, next_attempt_at, event_id)
    WHERE delivered_at IS NULL AND failed_at IS NULL;

COMMENT ON COLUMN webhook_deliveries.failed_at IS
    'When delivery stopped without success: a 410, or the endpoint disabled or deleted. `POST /v1/events/{id}/resend` restarts it.';
COMMENT ON COLUMN webhook_deliveries.url IS
    'The target of an endpoint''s notice of its own change, deletion, or test, delivered whatever the endpoint''s status; NULL: the endpoint''s current URL.';

ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN (
        'deposit', 'quote', 'api_key', 'account', 'refund', 'treasury', 'webhook_endpoint'
    ));

-- `GET /v1/events?type=` lists one account's events of a type, newest first.
CREATE INDEX events_scope_type_created_idx ON events (account_id, livemode, type, created, id);
