-- Restores the schema only. Deposits credited under webhook fulfillment stay credited; going back
-- past this migration is a restore from backup.
COMMENT ON COLUMN accounts.closed_at IS
    'Time the workspace closed, for operators only. No service decision reads it: late funds are refundable because the product answers rejected, recorded as product_refused.';

COMMENT ON TABLE settlements IS NULL;

DROP TRIGGER settlements_append_only ON settlements;

GRANT INSERT, UPDATE, DELETE ON TABLE settlements TO topup_app;

ALTER TABLE deposits
    DROP CONSTRAINT deposits_state_check,
    ADD CONSTRAINT deposits_state_check
        CHECK (state IN ('detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected'));
