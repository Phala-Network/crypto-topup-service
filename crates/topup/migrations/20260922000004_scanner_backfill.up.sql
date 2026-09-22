ALTER TABLE addresses
    ADD COLUMN created_block bigint NOT NULL DEFAULT 0 CHECK (created_block >= 0),
    ADD COLUMN backfilled boolean NOT NULL DEFAULT false;

CREATE INDEX addresses_pending_backfill_idx
    ON addresses (chain_id, created_block, id)
    WHERE backfilled = false;

COMMENT ON COLUMN addresses.created_block IS
    'Earliest block the scanner must inspect for this counterfactual address; zero is the conservative default for pre-C3 callers.';
COMMENT ON COLUMN addresses.backfilled IS
    'True after the scanner transaction has covered created_block through the chain cursor.';
