-- One deposit address per customer (docs/design/multi-tenant.md "Deposit addresses", the
-- 2026-09-28 amendment of D16): a deposit address no longer names a chain or an asset. Its salt
-- is the customer's and the version's, so its forwarder is the same address on every chain whose
-- treasury is the same address, and takes every supported token of that chain. The per-chain
-- forwarders are its `addresses` rows, one current row per chain; a treasury change on a chain
-- supersedes that chain's row with one over the new treasury, and the superseded row stays
-- watched and credited.
--
-- Rows of 20261007000000 name one chain and asset each and cannot be regrouped under one salt;
-- the migration refuses to run if any exists (none was issued before this change).

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM deposit_addresses) THEN
        RAISE EXCEPTION 'per-chain deposit addresses exist; they cannot be regrouped';
    END IF;
END;
$$;

DROP INDEX deposit_addresses_active_unique;
ALTER TABLE deposit_addresses DROP CONSTRAINT deposit_addresses_version_unique;
ALTER TABLE deposit_addresses DROP COLUMN chain_id;
ALTER TABLE deposit_addresses DROP COLUMN asset;
ALTER TABLE deposit_addresses DROP COLUMN route;
ALTER TABLE deposit_addresses ADD CONSTRAINT deposit_addresses_version_unique
    UNIQUE (customer_id, version);
-- At most one active address per customer and mode: creation returns it.
CREATE UNIQUE INDEX deposit_addresses_active_unique
    ON deposit_addresses (customer_id)
    WHERE status = 'active';

COMMENT ON TABLE deposit_addresses IS
    'A customer''s persistent deposit address on every chain and for every supported asset; retired by rotation, and still credited at spot when retired. Its forwarders are its addresses rows, one current row per chain.';
COMMENT ON COLUMN deposit_addresses.version IS
    'Counts the customer''s addresses from 1; each rotation takes the next. With the account, mode, and client_reference_id it derives the salt, which names no chain or asset.';

-- A deposit address has one forwarder row per chain, and another for each treasury the chain had
-- when the address was served; only the chain's current row is shown.
ALTER TABLE addresses DROP CONSTRAINT addresses_deposit_address_unique;
ALTER TABLE addresses ADD COLUMN superseded_at timestamptz;
ALTER TABLE addresses ADD CONSTRAINT addresses_superseded_check
    CHECK (superseded_at IS NULL OR deposit_address_id IS NOT NULL);
CREATE UNIQUE INDEX addresses_deposit_address_network_unique
    ON addresses (deposit_address_id, chain_id)
    WHERE deposit_address_id IS NOT NULL AND superseded_at IS NULL;
CREATE INDEX addresses_deposit_address_idx
    ON addresses (deposit_address_id)
    WHERE deposit_address_id IS NOT NULL;

COMMENT ON COLUMN addresses.deposit_address_id IS
    'The deposit address this forwarder is on its chain; null for a quote''s address.';
COMMENT ON COLUMN addresses.superseded_at IS
    'When a deposit address''s forwarder on this chain was replaced by one over the chain''s new treasury; still watched and credited. Null for the chain''s current forwarder and for quote addresses.';
