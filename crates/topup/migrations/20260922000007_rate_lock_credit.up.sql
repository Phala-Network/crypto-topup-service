ALTER TABLE rate_locks
    ADD COLUMN credit_minor bigint NOT NULL CHECK (credit_minor >= 0);
