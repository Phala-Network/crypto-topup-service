ALTER TABLE rate_locks
    ADD COLUMN credit_minor numeric(78,0);

DO $$
BEGIN
    IF EXISTS (SELECT 1 FROM rate_locks WHERE consumed_by IS NULL) THEN
        RAISE EXCEPTION
            'migration 20260922000008 requires every pre-existing rate lock to be consumed or removed';
    END IF;
END;
$$;

-- Historical consumed locks cannot be used again. Their original frozen credit was not stored,
-- so zero is a non-operative sentinel; all newly created locks persist the exact frozen credit.
UPDATE rate_locks
SET credit_minor = 0
WHERE consumed_by IS NOT NULL;

ALTER TABLE rate_locks
    ALTER COLUMN credit_minor SET NOT NULL,
    ADD CONSTRAINT rate_locks_credit_minor_check CHECK (credit_minor >= 0);
