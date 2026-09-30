-- The invariants the service assumes of its rows, as constraints, so a row that breaks one fails
-- the migration instead of a worker at runtime.

-- Every deposit the scanner records carries its transaction's sender and nonce, which the
-- confirm step and the finality watch read. Only a reversed deposit restored from a delivered
-- `deposit.reversed` (`restore_deposit_tombstones`) has neither: the delivery does not carry them,
-- and a reversed deposit is never confirmed or watched again.
ALTER TABLE deposits ADD CONSTRAINT deposits_tx_origin_check CHECK (
    (tx_from IS NULL) = (tx_nonce IS NULL) AND (tx_from IS NOT NULL OR state = 'reversed')
);

-- Every event's `data` holds its object, now checked for every row (20261015000000_api_conformance
-- checked only new ones), and a deposit's snapshot its receipt position, revision, and block: a
-- merchant's receiver and a restore's import read them.
ALTER TABLE events ALTER COLUMN data DROP DEFAULT;
ALTER TABLE events VALIDATE CONSTRAINT events_data_object_check;
ALTER TABLE events ADD CONSTRAINT events_deposit_identity_check CHECK (
    object_type <> 'deposit' OR (
        jsonb_typeof(data #> '{object,receipt_log_index}') IS NOT DISTINCT FROM 'number'
        AND jsonb_typeof(data #> '{object,revision}') IS NOT DISTINCT FROM 'number'
        AND jsonb_typeof(data #> '{object,block_hash}') IS NOT DISTINCT FROM 'string'
        AND jsonb_typeof(data #> '{object,block_time}') IS NOT DISTINCT FROM 'number'
    )
);
