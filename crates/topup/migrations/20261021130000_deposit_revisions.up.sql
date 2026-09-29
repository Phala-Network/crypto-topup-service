-- A reorganization can re-include a transaction against other state, so that a different transfer
-- sits at a deposit's receipt position. The finality watch reverses the old deposit, and the
-- transfer now at the position becomes a new deposit (architecture §7): the position identifies
-- one deposit that is not reversed, and each deposit recorded there has its own revision.
--
-- The unique constraint and index are built without CONCURRENTLY: `deposits` is locked for the
-- duration of the builds (writes wait; the constraint's rebuild also blocks reads). The deploy's
-- `migrate` service runs before `topup` starts; any other writer waits for it.
ALTER TABLE deposits
    ADD COLUMN revision bigint NOT NULL DEFAULT 0 CHECK (revision >= 0),
    ADD COLUMN replaces uuid,
    ADD CONSTRAINT deposits_replaces_unique UNIQUE (replaces),
    -- The replaced deposit is in the same account and mode, like every other deposit reference.
    ADD CONSTRAINT deposits_replaces_fkey FOREIGN KEY (replaces, account_id, livemode)
        REFERENCES deposits (id, account_id, livemode);

ALTER TABLE deposits DROP CONSTRAINT deposits_chain_event_unique;
ALTER TABLE deposits
    ADD CONSTRAINT deposits_chain_event_unique
        UNIQUE (chain_id, tx_hash, receipt_log_index, revision);
CREATE UNIQUE INDEX deposits_chain_event_live_unique
    ON deposits (chain_id, tx_hash, receipt_log_index)
    WHERE state <> 'reversed';

COMMENT ON COLUMN deposits.revision IS
    'How many deposits were recorded at the same receipt position before this one; each was reversed. The id is derived from chain_id, tx_hash, receipt_log_index, and a revision above 0.';
COMMENT ON COLUMN deposits.replaces IS
    'The reversed deposit of the same account and mode whose receipt position this deposit''s transfer took at finality (the API''s replaces; the other deposit''s replaced_by).';
