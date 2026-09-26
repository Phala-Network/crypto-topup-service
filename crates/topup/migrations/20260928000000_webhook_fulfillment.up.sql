-- Webhook fulfillment (docs/architecture.md §7, §11). A deposit that passes
-- screening is credited directly and the product is told with a `deposit.credited` webhook, so
-- the `cleared` state and the settlement protocol are retired. History stays: `transitions` keeps
-- `cleared`, `deposits.reason` keeps `product_refused`, and `settlements` stays as read-only audit.

-- In-flight `cleared` deposits return to `confirmed` with their stored valuation; the screen step
-- screens them again and credits them without re-quoting. A settlement the old step sent but
-- never recorded is harmless: the product deduplicates the new event by `deposit:<id>`.
INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
SELECT gen_random_uuid(), id, 'cleared', 'confirmed', 0,
       jsonb_build_object('migration', 'webhook_fulfillment')
FROM deposits
WHERE state = 'cleared';

UPDATE deposits
SET state = 'confirmed',
    attempt = 0,
    next_attempt_at = now(),
    lease_token = NULL,
    lease_until = NULL,
    updated_at = now()
WHERE state = 'cleared';

ALTER TABLE deposits
    DROP CONSTRAINT deposits_state_check,
    ADD CONSTRAINT deposits_state_check
        CHECK (state IN ('detected', 'confirmed', 'credited', 'swept', 'rejected'));

-- The settlement protocol's records are kept for the retention period and never written again.
REVOKE INSERT, UPDATE, DELETE ON TABLE settlements FROM topup_app;

CREATE TRIGGER settlements_append_only
BEFORE UPDATE OR DELETE ON settlements
FOR EACH ROW EXECUTE FUNCTION reject_append_only_mutation();

COMMENT ON TABLE settlements IS
    'Read-only history of the retired settlement protocol (docs/architecture.md §6); kept for the retention period.';

COMMENT ON COLUMN accounts.closed_at IS
    'Time the workspace closed, for operators only. No service decision reads it: the product refuses a credit for a closed workspace by requesting its refund.';
