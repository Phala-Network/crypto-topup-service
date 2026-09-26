DROP INDEX refunds_product_idempotency_key_unique;

ALTER TABLE refunds
    DROP COLUMN idempotency_key,
    DROP COLUMN product_id;
