-- Restores the per-chain, per-asset deposit addresses of 20261007000000. It refuses to run once a
-- deposit address was issued: its forwarders span chains and cannot be split by asset.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM deposit_addresses) THEN
        RAISE EXCEPTION 'deposit addresses exist; this migration cannot be reverted';
    END IF;
END;
$$;

DROP INDEX addresses_deposit_address_idx;
DROP INDEX addresses_deposit_address_network_unique;
ALTER TABLE addresses DROP CONSTRAINT addresses_superseded_check;
ALTER TABLE addresses DROP COLUMN superseded_at;
ALTER TABLE addresses ADD CONSTRAINT addresses_deposit_address_unique UNIQUE (deposit_address_id);
COMMENT ON COLUMN addresses.deposit_address_id IS
    'The deposit address this forwarder is; null for a quote''s address.';

DROP INDEX deposit_addresses_active_unique;
ALTER TABLE deposit_addresses DROP CONSTRAINT deposit_addresses_version_unique;
ALTER TABLE deposit_addresses ADD COLUMN chain_id bigint NOT NULL CHECK (chain_id >= 0);
ALTER TABLE deposit_addresses ADD COLUMN asset text NOT NULL
    CHECK (char_length(asset) BETWEEN 1 AND 64);
ALTER TABLE deposit_addresses ADD COLUMN route text NOT NULL;
ALTER TABLE deposit_addresses ADD CONSTRAINT deposit_addresses_version_unique
    UNIQUE (customer_id, chain_id, asset, version);
CREATE UNIQUE INDEX deposit_addresses_active_unique
    ON deposit_addresses (customer_id, chain_id, asset)
    WHERE status = 'active';

COMMENT ON TABLE deposit_addresses IS
    'A customer''s persistent deposit address for one chain and asset; retired by rotation, and still credited at spot when retired.';
COMMENT ON COLUMN deposit_addresses.asset IS
    'The route''s asset code; with the account, mode, client_reference_id, chain, and version it derives the salt.';
COMMENT ON COLUMN deposit_addresses.route IS
    'The route the address was issued on; its current version supplies the token contract of payment_uri.';
COMMENT ON COLUMN deposit_addresses.version IS
    'Counts the customer''s addresses for the chain and asset from 1; each rotation takes the next.';
