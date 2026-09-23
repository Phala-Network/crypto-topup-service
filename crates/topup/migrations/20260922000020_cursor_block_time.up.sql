ALTER TABLE cursors ADD COLUMN scanned_block_time timestamptz;

COMMENT ON COLUMN cursors.scanned_block_time IS
    'Block time of the finalized head when the scanner last committed through it; a lower bound on the time of scanned_block. Rate locks expire only once this passes expires_at.';

-- Rate-lock expiry waits while a payment mined inside the window is still unconfirmed.
CREATE INDEX deposits_detected_address_idx ON deposits (address_id) WHERE state = 'detected';
