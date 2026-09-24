#!/bin/sh
set -eu

# `keys` derives the application login's password from dstack `db/app/v1`. psql reads it from
# the login's pgpass file (field 5), so it never appears in argv or the environment.
app_pgpass=/run/db-app/topup_service.pgpass
if [ ! -s "$app_pgpass" ]; then
    echo "$app_pgpass is required to initialize the application login" >&2
    exit 1
fi

psql --username "$POSTGRES_USER" --dbname "$POSTGRES_DB" \
    --set=ON_ERROR_STOP=1 --set=app_pgpass="$app_pgpass" <<'SQL'
\set app_password `cut -d: -f5 :'app_pgpass'`

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
