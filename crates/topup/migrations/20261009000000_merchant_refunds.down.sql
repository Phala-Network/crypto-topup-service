-- Restores the operator refund workflow's columns and claim table as
-- 20261004000000_multi_tenant created them. Failed and canceled refunds have no equivalent and
-- are discarded with their `refund.failed` events; succeeded refunds become `confirmed` with
-- their transfer log as the claim.

DELETE FROM webhook_deliveries
WHERE event_id IN (SELECT id FROM events WHERE object_type = 'refund');
DELETE FROM events WHERE object_type = 'refund';
ALTER TABLE events DROP CONSTRAINT events_object_type_check;
ALTER TABLE events ADD CONSTRAINT events_object_type_check
    CHECK (object_type IN ('deposit', 'quote', 'api_key', 'account'));

DELETE FROM refunds WHERE status IN ('failed', 'canceled');

DROP INDEX refunds_verification_due_idx;
DROP INDEX refunds_deposit_idx;
DROP INDEX refunds_transfer_unique;

ALTER TABLE refunds
    DROP CONSTRAINT refunds_succeeded_transfer_check,
    DROP CONSTRAINT refunds_log_index_tx_hash_check,
    DROP CONSTRAINT refunds_failure_reason_check,
    DROP CONSTRAINT refunds_status_check,
    DROP CONSTRAINT refunds_amount_atomic_check,
    ADD CONSTRAINT refunds_amount_atomic_check CHECK (amount_atomic >= 0),
    ADD COLUMN requested_by text NOT NULL DEFAULT 'migration',
    ADD COLUMN approved_by text,
    ADD COLUMN confirmed_at timestamptz,
    ADD COLUMN tx_version bigint NOT NULL DEFAULT 0 CHECK (tx_version >= 0),
    ADD COLUMN route text;
ALTER TABLE refunds ALTER COLUMN requested_by DROP DEFAULT;

UPDATE refunds
SET route = COALESCE(deposit.route, ''),
    approved_by = CASE WHEN refunds.tx_hash IS NULL THEN NULL ELSE 'migration' END,
    tx_version = CASE WHEN refunds.tx_hash IS NULL THEN 0 ELSE 1 END,
    confirmed_at = CASE WHEN refunds.status = 'succeeded' THEN refunds.updated_at END,
    status = CASE
        WHEN refunds.status = 'succeeded' THEN 'confirmed'
        WHEN refunds.tx_hash IS NOT NULL THEN 'sent'
        ELSE 'requested'
    END
FROM deposits AS deposit
WHERE deposit.id = refunds.deposit_id;
ALTER TABLE refunds
    ALTER COLUMN route SET NOT NULL,
    ADD CONSTRAINT refunds_status_check
        CHECK (status IN ('requested', 'approved', 'sent', 'confirmed'));

ALTER TABLE refunds RENAME CONSTRAINT refunds_destination_address_canonical_hex_check
    TO refunds_to_address_canonical_hex_check;
ALTER TABLE refunds RENAME COLUMN destination_address TO to_address;

CREATE UNIQUE INDEX refunds_idempotency_unique
    ON refunds (deposit_id, to_address, amount_atomic);
CREATE INDEX refunds_confirmation_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'sent';

CREATE TABLE refund_payment_claims (
    refund_id uuid NOT NULL REFERENCES refunds(id),
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    claimed_amount_atomic numeric(78,0) NOT NULL CHECK (claimed_amount_atomic > 0),
    transferred_amount_atomic numeric(78,0) NOT NULL CHECK (transferred_amount_atomic > 0),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (refund_id, chain_id, tx_hash, log_index),
    CONSTRAINT refund_payment_claims_log_unique UNIQUE (chain_id, tx_hash, log_index)
);

INSERT INTO refund_payment_claims (
    refund_id, chain_id, tx_hash, log_index, claimed_amount_atomic, transferred_amount_atomic
)
SELECT id, chain_id, tx_hash, log_index, amount_atomic, amount_atomic
FROM refunds
WHERE status = 'confirmed';

CREATE FUNCTION enforce_refund_payment_claim_capacity()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    claimed numeric(78,0);
BEGIN
    SELECT COALESCE(sum(claimed_amount_atomic), 0)
      INTO claimed
      FROM refund_payment_claims
     WHERE chain_id = NEW.chain_id
       AND tx_hash = NEW.tx_hash
       AND log_index = NEW.log_index;
    IF claimed > 0 THEN
        RETURN NEW;
    END IF;
    IF claimed + NEW.claimed_amount_atomic > NEW.transferred_amount_atomic THEN
        RAISE EXCEPTION 'refund payment claim exceeds transfer log amount'
            USING ERRCODE = '23514';
    END IF;
    RETURN NEW;
END;
$$;

CREATE TRIGGER refund_payment_claim_capacity
BEFORE INSERT ON refund_payment_claims
FOR EACH ROW EXECUTE FUNCTION enforce_refund_payment_claim_capacity();

COMMENT ON TABLE refund_payment_claims IS
    'Atomic allocation of finalized ERC-20 transfer logs to confirmed refunds.';

ALTER TABLE refunds
    DROP COLUMN failure_reason,
    DROP COLUMN log_index,
    DROP COLUMN chain_id;
