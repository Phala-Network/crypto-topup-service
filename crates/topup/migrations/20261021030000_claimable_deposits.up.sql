-- The pump claims only `detected` and `confirmed` deposits. A credited deposit waits for a
-- finalized `Flushed` event, which the scanner and the finality watch apply; the pump used to claim
-- it every wait interval only to record another no-op wait. The claim index follows the claim, so
-- credited deposits that are never swept do not accumulate at the head of every claim's scan.
DROP INDEX deposits_claimable_idx;

CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state IN ('detected', 'confirmed');
