-- Display-only transfers seen above the finalized head (architecture §8, §12). Rows never feed
-- deposits, transitions, rate locks, exposure, settlement, or reconciliation; the finalized
-- scanner deletes them in the same transaction that advances its cursor past their block.
CREATE TABLE pending_transfers (
    chain_id bigint NOT NULL CHECK (chain_id >= 0),
    tx_hash text NOT NULL,
    log_index bigint NOT NULL CHECK (log_index >= 0),
    block_number bigint NOT NULL CHECK (block_number >= 0),
    block_hash text NOT NULL,
    block_time timestamptz NOT NULL,
    head_block bigint NOT NULL,
    address_id uuid NOT NULL REFERENCES addresses(id),
    asset_contract text NOT NULL,
    from_address text NOT NULL,
    amount_atomic numeric(78,0) NOT NULL CHECK (amount_atomic >= 0),
    first_seen_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chain_id, tx_hash, log_index),
    CONSTRAINT pending_transfers_head_check CHECK (head_block >= block_number)
);

CREATE INDEX pending_transfers_address_idx
    ON pending_transfers (address_id, block_number, log_index);
CREATE INDEX pending_transfers_chain_block_idx
    ON pending_transfers (chain_id, block_number);

REVOKE ALL PRIVILEGES ON TABLE pending_transfers FROM topup_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE pending_transfers TO topup_app;

COMMENT ON COLUMN pending_transfers.head_block IS
    'Provider A latest block at the last head scan that saw this transfer; confirmations = head_block - block_number + 1.';

-- Last time the product issued or fetched this address; bounds the persistent addresses the head
-- scan watches when there are more than one log request can carry.
ALTER TABLE addresses ADD COLUMN requested_at timestamptz NOT NULL DEFAULT now();

CREATE INDEX addresses_persistent_requested_idx
    ON addresses (chain_id, requested_at)
    WHERE kind = 'persistent';
