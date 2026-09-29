-- A reorganization can re-include a transaction against other state, so that a different transfer
-- sits at a deposit's receipt position. The finality watch reverses the old deposit, and the
-- transfer now at the position becomes a new deposit (architecture §7): the position identifies
-- one deposit that is not reversed, and each deposit recorded there has its own revision.
ALTER TABLE deposits
    ADD COLUMN revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0);

ALTER TABLE deposits DROP CONSTRAINT deposits_chain_event_unique;
ALTER TABLE deposits
    ADD CONSTRAINT deposits_chain_event_unique
        UNIQUE (chain_id, tx_hash, receipt_log_index, revision);
CREATE UNIQUE INDEX deposits_chain_event_live_unique
    ON deposits (chain_id, tx_hash, receipt_log_index)
    WHERE state <> 'reversed';

COMMENT ON COLUMN deposits.revision IS
    'How many deposits were recorded at the same receipt position before this one; each was reversed. The id is derived from chain_id, tx_hash, receipt_log_index, and a revision above 0.';
