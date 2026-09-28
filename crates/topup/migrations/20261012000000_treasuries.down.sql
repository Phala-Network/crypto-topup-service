-- Restores the unused `treasuries` table of 20261004000000. It refuses to run once a treasury was
-- set: forwarders were issued over it.

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM treasuries) THEN
        RAISE EXCEPTION 'treasuries exist; this migration cannot be reverted';
    END IF;
END;
$$;

ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote', 'api_key', 'account', 'refund'));

DROP INDEX addresses_deposit_address_chain_idx;
DROP TABLE treasury_challenges;

DROP INDEX treasuries_scope_idx;
DROP INDEX treasuries_due_idx;
DROP INDEX treasuries_current_unique;
DROP INDEX treasuries_pending_unique;
ALTER TABLE treasuries DROP CONSTRAINT treasuries_lifecycle_check;
ALTER TABLE treasuries DROP CONSTRAINT treasuries_proof_signature_check;
ALTER TABLE treasuries ALTER COLUMN created_by DROP NOT NULL;
ALTER TABLE treasuries ALTER COLUMN screened_at DROP NOT NULL;
ALTER TABLE treasuries ALTER COLUMN effective_at DROP NOT NULL;
ALTER TABLE treasuries ALTER COLUMN verified_at DROP NOT NULL;
ALTER TABLE treasuries DROP COLUMN replaced_at;
ALTER TABLE treasuries DROP COLUMN applied_at;
ALTER TABLE treasuries DROP COLUMN kind;
ALTER TABLE treasuries DROP COLUMN livemode;
COMMENT ON TABLE treasuries IS NULL;
CREATE INDEX treasuries_account_chain_idx ON treasuries (account_id, chain_id, effective_at);
