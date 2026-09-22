DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_app') THEN
        CREATE ROLE topup_app NOLOGIN;
    END IF;
END;
$$;

GRANT USAGE ON SCHEMA public TO topup_app;

REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM topup_app;
GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE
    products,
    accounts,
    addresses,
    rate_locks,
    cursors,
    deposits,
    settlements,
    flushes,
    flushed,
    refunds,
    outbox
TO topup_app;
GRANT SELECT, INSERT ON TABLE transitions, audit TO topup_app;
REVOKE TRUNCATE ON ALL TABLES IN SCHEMA public FROM topup_app;

ALTER DEFAULT PRIVILEGES IN SCHEMA public
    GRANT SELECT, INSERT, UPDATE, DELETE ON TABLES TO topup_app;
ALTER DEFAULT PRIVILEGES IN SCHEMA public
    REVOKE TRUNCATE ON TABLES FROM topup_app;

COMMENT ON FUNCTION reject_append_only_mutation() IS
    'Defense-in-depth append-only enforcement; trusted table owners and superusers can disable or bypass triggers.';
