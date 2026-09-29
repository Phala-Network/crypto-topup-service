-- Restores the claim index before 20261021030000_claimable_deposits.
DROP INDEX deposits_claimable_idx;

CREATE INDEX deposits_claimable_idx
    ON deposits (next_attempt_at, created_at, id)
    WHERE state NOT IN ('swept', 'rejected', 'reversed');
