ALTER TABLE refunds
    DROP CONSTRAINT refunds_amount_atomic_integer_check,
    DROP CONSTRAINT refunds_tx_hash_canonical_hex_check,
    DROP CONSTRAINT refunds_to_address_canonical_hex_check;

ALTER TABLE flushed
    DROP CONSTRAINT flushed_amount_atomic_integer_check;

ALTER TABLE flushes
    DROP CONSTRAINT flushes_nonce_integer_check,
    DROP CONSTRAINT flushes_tx_hash_canonical_hex_check,
    DROP CONSTRAINT flushes_operator_canonical_hex_check,
    DROP CONSTRAINT flushes_token_canonical_hex_check;

ALTER TABLE deposits
    DROP CONSTRAINT deposits_credit_minor_integer_check,
    DROP CONSTRAINT deposits_price_scaled_integer_check,
    DROP CONSTRAINT deposits_amount_atomic_integer_check,
    DROP CONSTRAINT deposits_from_address_canonical_hex_check,
    DROP CONSTRAINT deposits_asset_contract_canonical_hex_check,
    DROP CONSTRAINT deposits_block_hash_canonical_hex_check,
    DROP CONSTRAINT deposits_tx_hash_canonical_hex_check;

ALTER TABLE rate_locks
    DROP CONSTRAINT rate_locks_price_scaled_integer_check,
    DROP CONSTRAINT rate_locks_amount_atomic_integer_check;

ALTER TABLE addresses
    DROP CONSTRAINT addresses_address_canonical_hex_check,
    DROP CONSTRAINT addresses_salt_canonical_hex_check;
