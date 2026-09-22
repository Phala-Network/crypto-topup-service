#!/bin/sh
set -eu

if [ -z "${TOPUP_APP_PASSWORD:-}" ]; then
    echo "TOPUP_APP_PASSWORD is required to initialize the application login" >&2
    exit 1
fi

psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
    --set=ON_ERROR_STOP=1 --set=app_password="$TOPUP_APP_PASSWORD" <<'SQL'
DO $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_app') THEN
        CREATE ROLE topup_app NOLOGIN;
    END IF;
END;
$$;

SELECT format(
    'CREATE ROLE topup_service LOGIN PASSWORD %L IN ROLE topup_app',
    :'app_password'
)
WHERE NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_service')
\gexec

SELECT format('ALTER ROLE topup_service PASSWORD %L', :'app_password')
WHERE EXISTS (SELECT 1 FROM pg_roles WHERE rolname = 'topup_service')
\gexec
SQL
