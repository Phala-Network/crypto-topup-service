-- The attested route's `destination.settlement_url` and `destination.product_kid` are the only
-- source of a product's settlement endpoint and request key id (docs/architecture.md §14).
ALTER TABLE products
    DROP COLUMN settlement_url,
    DROP COLUMN kid;
