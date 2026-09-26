DROP INDEX rate_locks_product_idempotency_key_unique;

ALTER TABLE rate_locks
    DROP COLUMN idempotency_key,
    DROP COLUMN product_id;
