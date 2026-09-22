ALTER TABLE refunds
    ADD COLUMN confirmation_evidence jsonb,
    ADD COLUMN next_check_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN updated_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN confirmed_at timestamptz,
    ADD COLUMN tx_version bigint NOT NULL DEFAULT 0 CHECK (tx_version >= 0);

ALTER TABLE accounts
    ADD COLUMN status text NOT NULL DEFAULT 'active'
        CHECK (status IN ('active', 'closed'));

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
    -- The global unique constraint owns reuse conflicts. Returning here lets
    -- INSERT ... ON CONFLICT report an unavailable claim without aborting the worker.
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

CREATE UNIQUE INDEX refunds_idempotency_unique
    ON refunds (deposit_id, to_address, amount_atomic);

CREATE INDEX refunds_confirmation_due_idx
    ON refunds (next_check_at, id)
    WHERE status = 'sent';

COMMENT ON COLUMN refunds.confirmation_evidence IS
    'Most recent finalized transaction verification evidence, including mismatches.';
COMMENT ON COLUMN refunds.next_check_at IS
    'Earliest time the background confirmation worker should inspect a sent refund.';
COMMENT ON TABLE refund_payment_claims IS
    'Atomic allocation of finalized ERC-20 transfer logs to confirmed refunds.';
