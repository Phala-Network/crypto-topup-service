-- Stripe-style metadata on quotes, deposits, and refunds (docs/design/multi-tenant.md, Metadata;
-- https://docs.stripe.com/api/metadata): up to 50 string key/value pairs, keys of 1 to 40
-- characters without square brackets, values of 1 to 500 characters. An empty value unsets a key,
-- so none is stored. The API validates first and answers `400` naming `metadata[key]`; the CHECK
-- is the invariant behind it.

CREATE FUNCTION metadata_is_valid(metadata jsonb)
RETURNS boolean
LANGUAGE sql
IMMUTABLE
PARALLEL SAFE
AS $$
    SELECT CASE
        WHEN jsonb_typeof(metadata) <> 'object' THEN false
        ELSE (SELECT count(*) <= 50 FROM jsonb_object_keys(metadata))
            AND NOT EXISTS (
                SELECT 1
                FROM jsonb_each(metadata) AS entry (key, value)
                WHERE char_length(entry.key) NOT BETWEEN 1 AND 40
                   OR strpos(entry.key, '[') > 0
                   OR strpos(entry.key, ']') > 0
                   OR jsonb_typeof(entry.value) <> 'string'
                   OR char_length(entry.value #>> '{}') NOT BETWEEN 1 AND 500
            )
    END
$$;

COMMENT ON FUNCTION metadata_is_valid(jsonb) IS
    'Whether a metadata column value is a Stripe metadata object: at most 50 string pairs, keys of 1 to 40 characters without [ or ], values of 1 to 500 characters.';

ALTER TABLE quotes ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}'
    CONSTRAINT quotes_metadata_check CHECK (metadata_is_valid(metadata));
ALTER TABLE deposits ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}'
    CONSTRAINT deposits_metadata_check CHECK (metadata_is_valid(metadata));
ALTER TABLE refunds ADD COLUMN metadata jsonb NOT NULL DEFAULT '{}'
    CONSTRAINT refunds_metadata_check CHECK (metadata_is_valid(metadata));

COMMENT ON COLUMN quotes.metadata IS
    'The merchant''s key/value pairs; set on create and POST /v1/quotes/{id}. Never read by the service.';
COMMENT ON COLUMN deposits.metadata IS
    'The merchant''s key/value pairs, a copy of the quote''s when the deposit is recorded and independent after; updated by POST /v1/deposits/{id}.';
COMMENT ON COLUMN refunds.metadata IS
    'The merchant''s key/value pairs; set on create and POST /v1/refunds/{id}.';

-- Updating a deposit's metadata is the one deposit write a key makes; it is held by whoever holds
-- `quotes.write`, whichever principals the table has when this runs.
INSERT INTO permissions (permission, principal)
SELECT 'deposits.write', principal
FROM permissions
WHERE permission = 'quotes.write';
