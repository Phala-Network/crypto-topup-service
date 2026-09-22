ALTER TABLE rate_locks
    ADD COLUMN status text NOT NULL DEFAULT 'open',
    ADD COLUMN exposure_reserved boolean NOT NULL DEFAULT false,
    ADD COLUMN created_at timestamptz NOT NULL DEFAULT now(),
    ADD COLUMN closed_at timestamptz;

UPDATE rate_locks
SET status = 'consumed', closed_at = now()
WHERE consumed_by IS NOT NULL;

ALTER TABLE rate_locks
    ADD CONSTRAINT rate_locks_status_check
        CHECK (status IN ('open', 'consumed', 'expired', 'cancelled')),
    ADD CONSTRAINT rate_locks_status_consumption_check CHECK (
        (status = 'consumed' AND consumed_by IS NOT NULL)
        OR (status <> 'consumed' AND consumed_by IS NULL)
    ),
    ADD CONSTRAINT rate_locks_closed_at_check CHECK (
        (status = 'open' AND closed_at IS NULL)
        OR (status <> 'open' AND closed_at IS NOT NULL)
    );

CREATE UNIQUE INDEX addresses_account_lock_ref_unique
    ON addresses (account_id, lock_ref)
    WHERE kind = 'lock';

CREATE INDEX rate_locks_open_expiry_idx
    ON rate_locks (expires_at, address_id)
    WHERE status = 'open';

CREATE TABLE lock_exposure (
    scope_key text PRIMARY KEY,
    open_minor numeric(78,0) NOT NULL CHECK (open_minor >= 0),
    updated_at timestamptz NOT NULL DEFAULT now()
);

GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE lock_exposure TO topup_app;
REVOKE TRUNCATE ON TABLE lock_exposure FROM topup_app;
