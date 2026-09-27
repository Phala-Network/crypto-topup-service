-- Stripe-style events (docs/architecture.md §12). An event names its product and the
-- object it is about; its `data`, `{"object": …}`, is the object's API representation, rendered
-- on the first delivery attempt and stored in `payload`, never re-rendered. Rows written before
-- this migration are format 1: their payload is the old flat `data` and they are delivered in the
-- old envelope, so a replay stays byte-identical.
DO $$
BEGIN
    IF EXISTS (
        SELECT 1 FROM outbox WHERE event_type = 'deposit.credited' AND delivered_at IS NULL
    ) THEN
        RAISE EXCEPTION 'undelivered deposit.credited events remain; deliver them before this migration';
    END IF;
END
$$;

ALTER TABLE outbox
    ADD COLUMN format smallint NOT NULL DEFAULT 1 CHECK (format IN (1, 2)),
    ADD COLUMN product_id uuid REFERENCES products(id),
    ADD COLUMN object_type text CHECK (object_type IN ('deposit', 'quote')),
    ADD COLUMN object_id uuid,
    ADD CONSTRAINT outbox_format_object_check CHECK (
        format = 1 OR (product_id IS NOT NULL AND object_type IS NOT NULL AND object_id IS NOT NULL)
    );
ALTER TABLE outbox ALTER COLUMN format SET DEFAULT 2;

UPDATE outbox SET product_id = (payload ->> 'product_id')::uuid
WHERE payload ? 'product_id';

CREATE INDEX outbox_object_idx ON outbox (object_id) WHERE object_id IS NOT NULL;

COMMENT ON COLUMN outbox.payload IS
    'Format 2: the event data, {"object": …}, rendered on the first delivery attempt ({} until then). Format 1: the old flat data.';
