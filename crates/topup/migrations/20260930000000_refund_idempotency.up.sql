-- Refunds (docs/architecture.md §12): `POST /v1/refunds` takes an Idempotency-Key,
-- stored on the refund and unique per product. Refunds created before this migration have no key.
ALTER TABLE refunds
    ADD COLUMN product_id uuid REFERENCES products(id),
    ADD COLUMN idempotency_key text
        CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255),
    ADD CONSTRAINT refunds_idempotency_key_product_check
        CHECK (idempotency_key IS NULL OR product_id IS NOT NULL);

CREATE UNIQUE INDEX refunds_product_idempotency_key_unique
    ON refunds (product_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
