-- Quotes (docs/architecture.md §9). A rate lock is the API's quote: its id is
-- `qt_` + the hex of `address_id`, and a new quote's address salt uses that id as its reference.
-- `Idempotency-Key` replaces the product's lock reference for retries, so the key is stored on the
-- quote, unique per product: `product_id` is the owning account's product, recorded with the key
-- for that index. A quote's `client_secret` lets a browser read it without the product's
-- signature; only its SHA-256 is stored. Rows created before this migration keep their references
-- and salts and have no key and no client secret.
ALTER TABLE rate_locks
    ADD COLUMN product_id uuid REFERENCES products(id),
    ADD COLUMN idempotency_key text
        CHECK (idempotency_key IS NULL OR octet_length(idempotency_key) BETWEEN 1 AND 255),
    ADD COLUMN client_secret_hash bytea
        CHECK (client_secret_hash IS NULL OR octet_length(client_secret_hash) = 32),
    ADD CONSTRAINT rate_locks_idempotency_key_product_check
        CHECK (idempotency_key IS NULL OR product_id IS NOT NULL);

CREATE UNIQUE INDEX rate_locks_product_idempotency_key_unique
    ON rate_locks (product_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

COMMENT ON COLUMN rate_locks.idempotency_key IS
    'The Idempotency-Key the quote was created with; a repeat with the same parameters returns this quote.';

COMMENT ON COLUMN rate_locks.client_secret_hash IS
    'SHA-256 of the quote''s client_secret, the bearer of its public read; replaced when an Idempotency-Key repeat returns a new secret. NULL for quotes created before client secrets.';
