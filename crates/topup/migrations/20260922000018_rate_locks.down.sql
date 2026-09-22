DROP TABLE lock_exposure;

DROP INDEX rate_locks_open_expiry_idx;
DROP INDEX addresses_account_lock_ref_unique;

ALTER TABLE rate_locks
    DROP CONSTRAINT rate_locks_closed_at_check,
    DROP CONSTRAINT rate_locks_status_consumption_check,
    DROP CONSTRAINT rate_locks_status_check,
    DROP COLUMN closed_at,
    DROP COLUMN created_at,
    DROP COLUMN exposure_reserved,
    DROP COLUMN status;
