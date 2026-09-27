-- Restores the schema only. Reversed deposits have no earlier state and block the down migration.
DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM deposits WHERE state = 'reversed') THEN
        RAISE EXCEPTION 'reversed deposits exist; the fast-credit migration cannot be reverted';
    END IF;
END
$$;

COMMENT ON COLUMN cursors.scanned_block IS NULL;
ALTER TABLE cursors DROP COLUMN confirmed_block;

ALTER TABLE pending_transfers DROP COLUMN receipt_log_index;

DROP INDEX deposits_unfinal_idx;
DROP INDEX deposits_claimable_idx;
CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state NOT IN ('swept', 'rejected');

ALTER TABLE transitions
    DROP CONSTRAINT transitions_from_state_check,
    ADD CONSTRAINT transitions_from_state_check CHECK (from_state IN (
        'detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected'
    )),
    DROP CONSTRAINT transitions_to_state_check,
    ADD CONSTRAINT transitions_to_state_check CHECK (to_state IN (
        'detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected'
    ));

ALTER TABLE deposits
    DROP CONSTRAINT deposits_swept_final_check,
    DROP CONSTRAINT deposits_state_check,
    ADD CONSTRAINT deposits_state_check
        CHECK (state IN ('detected', 'confirmed', 'credited', 'swept', 'rejected')),
    DROP CONSTRAINT deposits_chain_event_unique,
    ADD CONSTRAINT deposits_chain_event_unique UNIQUE (chain_id, tx_hash, log_index),
    DROP COLUMN final_at,
    DROP COLUMN tx_nonce,
    DROP COLUMN tx_from,
    DROP COLUMN receipt_log_index;
