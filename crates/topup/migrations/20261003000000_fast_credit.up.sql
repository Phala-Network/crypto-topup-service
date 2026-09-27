-- Fast credit and reversal (docs/design/multi-tenant.md §4, docs/architecture.md §6, §7). A
-- deposit is credited at the route's confirmation and watched to finality; its identity is its
-- transaction and its log's position in the transaction's receipt, which survives re-inclusion.
--
-- Rows written before this migration were born final and keep the block-wide log index as their
-- receipt position: their ids were derived from it, and staging is reset before multi-tenancy
-- (design §16, PR 13), so no id is migrated. They are marked final, so the finality watch never
-- reads them.

ALTER TABLE deposits
    ADD COLUMN receipt_log_index bigint CHECK (receipt_log_index >= 0),
    ADD COLUMN tx_from text CHECK (tx_from ~ '^0x[0-9a-f]{40}$'),
    ADD COLUMN tx_nonce numeric(20,0) CHECK (tx_nonce >= 0 AND tx_nonce = trunc(tx_nonce)),
    ADD COLUMN final_at timestamptz;

UPDATE deposits
SET receipt_log_index = log_index,
    final_at = COALESCE(valuation_at, created_at);

ALTER TABLE deposits
    ALTER COLUMN receipt_log_index SET NOT NULL,
    DROP CONSTRAINT deposits_chain_event_unique,
    ADD CONSTRAINT deposits_chain_event_unique UNIQUE (chain_id, tx_hash, receipt_log_index),
    DROP CONSTRAINT deposits_state_check,
    ADD CONSTRAINT deposits_state_check
        CHECK (state IN ('detected', 'confirmed', 'credited', 'swept', 'rejected', 'reversed')),
    -- A deposit is swept only through a finalized `Flushed` event after its final log position.
    ADD CONSTRAINT deposits_swept_final_check CHECK (state <> 'swept' OR final_at IS NOT NULL);

ALTER TABLE transitions
    DROP CONSTRAINT transitions_from_state_check,
    ADD CONSTRAINT transitions_from_state_check CHECK (from_state IN (
        'detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected', 'reversed'
    )),
    DROP CONSTRAINT transitions_to_state_check,
    ADD CONSTRAINT transitions_to_state_check CHECK (to_state IN (
        'detected', 'confirmed', 'cleared', 'credited', 'swept', 'rejected', 'reversed'
    ));

DROP INDEX deposits_claimable_idx;
CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state NOT IN ('swept', 'rejected', 'reversed');

-- The finality watch reads every deposit that is neither final nor reversed.
CREATE INDEX deposits_unfinal_idx
    ON deposits (chain_id, block_number)
    WHERE final_at IS NULL AND state <> 'reversed';

COMMENT ON COLUMN deposits.receipt_log_index IS
    'Position of the transfer log among the logs of its transaction''s receipt; with chain_id and tx_hash the deposit identity. log_index, block_number, and block_hash are evidence that follows re-inclusion.';
COMMENT ON COLUMN deposits.tx_from IS
    'Sender of the transaction; with tx_nonce, proves a dropped transaction once another consumed its nonce.';
COMMENT ON COLUMN deposits.final_at IS
    'When both providers showed the transfer at or below finalized. Null while the deposit can still be reversed.';

-- The head scan's display-only rows name the deposit id they will have, so they carry the receipt
-- position too. They are rebuilt by the next head scan.
DELETE FROM pending_transfers;
ALTER TABLE pending_transfers
    ADD COLUMN receipt_log_index bigint NOT NULL CHECK (receipt_log_index >= 0);

-- The fast scanner's cursor: the highest block scanned at the route's confirmation. It never
-- trails `scanned_block` in effect: the fast scan starts above both.
ALTER TABLE cursors
    ADD COLUMN confirmed_block bigint CHECK (confirmed_block >= 0);

COMMENT ON COLUMN cursors.scanned_block IS
    'Last block the finalized scanner committed through. Rate-lock expiry, reconciliation, and the display-only pending view read it.';
COMMENT ON COLUMN cursors.confirmed_block IS
    'Last block the fast scanner committed through at the route confirmation (a depth or safe); null when the chain credits only at finalized.';
