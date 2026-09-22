ALTER TABLE addresses
    ADD CONSTRAINT addresses_salt_canonical_hex_check
        CHECK (salt ~ '^0x[0-9a-f]{64}$'),
    ADD CONSTRAINT addresses_address_canonical_hex_check
        CHECK (address ~ '^0x[0-9a-f]{40}$');

ALTER TABLE rate_locks
    ADD CONSTRAINT rate_locks_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$'),
    ADD CONSTRAINT rate_locks_price_scaled_integer_check
        CHECK (price_scaled = trunc(price_scaled) AND price_scaled::text ~ '^[0-9]+$');

ALTER TABLE deposits
    ADD CONSTRAINT deposits_tx_hash_canonical_hex_check
        CHECK (tx_hash ~ '^0x[0-9a-f]{64}$'),
    ADD CONSTRAINT deposits_block_hash_canonical_hex_check
        CHECK (block_hash ~ '^0x[0-9a-f]{64}$'),
    ADD CONSTRAINT deposits_asset_contract_canonical_hex_check
        CHECK (asset_contract ~ '^0x[0-9a-f]{40}$'),
    ADD CONSTRAINT deposits_from_address_canonical_hex_check
        CHECK (from_address ~ '^0x[0-9a-f]{40}$'),
    ADD CONSTRAINT deposits_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$'),
    ADD CONSTRAINT deposits_price_scaled_integer_check
        CHECK (
            price_scaled IS NULL
            OR (price_scaled = trunc(price_scaled) AND price_scaled::text ~ '^[0-9]+$')
        ),
    ADD CONSTRAINT deposits_credit_minor_integer_check
        CHECK (
            credit_minor IS NULL
            OR (credit_minor = trunc(credit_minor) AND credit_minor::text ~ '^[0-9]+$')
        );

ALTER TABLE flushes
    ADD CONSTRAINT flushes_token_canonical_hex_check
        CHECK (token ~ '^0x[0-9a-f]{40}$'),
    ADD CONSTRAINT flushes_operator_canonical_hex_check
        CHECK (operator ~ '^0x[0-9a-f]{40}$'),
    ADD CONSTRAINT flushes_tx_hash_canonical_hex_check
        CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    ADD CONSTRAINT flushes_nonce_integer_check
        CHECK (nonce = trunc(nonce) AND nonce::text ~ '^[0-9]+$');

ALTER TABLE flushed
    ADD CONSTRAINT flushed_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$');

ALTER TABLE refunds
    ADD CONSTRAINT refunds_to_address_canonical_hex_check
        CHECK (to_address ~ '^0x[0-9a-f]{40}$'),
    ADD CONSTRAINT refunds_tx_hash_canonical_hex_check
        CHECK (tx_hash IS NULL OR tx_hash ~ '^0x[0-9a-f]{64}$'),
    ADD CONSTRAINT refunds_amount_atomic_integer_check
        CHECK (amount_atomic = trunc(amount_atomic) AND amount_atomic::text ~ '^[0-9]+$');
