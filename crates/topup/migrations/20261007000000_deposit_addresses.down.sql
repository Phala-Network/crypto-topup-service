-- Restores the schema before 20261007000000_deposit_addresses. It refuses to run once a deposit
-- address was issued: its forwarder, deposits, and their append-only history would otherwise
-- lose their owner, and funds sent to it would stop being recorded.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM deposit_addresses) THEN
        RAISE EXCEPTION 'deposit addresses exist; this migration cannot be reverted';
    END IF;
END;
$$;

DELETE FROM permissions WHERE permission IN ('deposit_addresses.read', 'deposit_addresses.write');
ALTER TABLE account_limits DROP COLUMN max_active_deposit_addresses;
ALTER TABLE addresses DROP CONSTRAINT addresses_owner_check;
ALTER TABLE addresses DROP COLUMN deposit_address_id;
ALTER TABLE addresses ALTER COLUMN quote_id SET NOT NULL;
DROP TABLE deposit_addresses;
