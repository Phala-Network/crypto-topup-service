ALTER DEFAULT PRIVILEGES IN SCHEMA public
    REVOKE SELECT, INSERT, UPDATE, DELETE, TRUNCATE ON TABLES FROM topup_app;
REVOKE ALL PRIVILEGES ON ALL TABLES IN SCHEMA public FROM topup_app;
REVOKE USAGE ON SCHEMA public FROM topup_app;
DROP ROLE IF EXISTS topup_app;

COMMENT ON FUNCTION reject_append_only_mutation() IS
    'Trigger enforcement protects append-only history for every database role, including table owners.';
