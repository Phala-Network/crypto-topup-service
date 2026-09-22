#!/bin/sh
# Starts the postgres-walg image and proves TOPUP_WAL_ARCHIVE controls archiving, even against
# user-supplied flags, so a restore drill cannot write into the production WAL prefix.
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
prefix="topup-archive-switch-$$"
containers=
built_image=

cleanup() {
    for container in $containers; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
    if [ -n "$built_image" ]; then
        docker image rm "$built_image" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT INT TERM

# Test the given image (CI passes the one it just built); otherwise build a per-run tag from this
# checkout, because a shared tag such as crypto-topup-postgres-walg:local may be stale.
if [ "$#" -ge 1 ]; then
    image=$1
else
    built_image="crypto-topup-postgres-walg:$prefix"
    docker build -q -f "$root/deploy/Dockerfile.postgres-walg" -t "$built_image" "$root" >/dev/null
    image=$built_image
fi

start() {
    name="$prefix-$1"
    shift
    containers="$containers $name"
    docker run -d --name "$name" -e POSTGRES_PASSWORD=postgres "$@" >/dev/null
    attempts=60
    # The init-time temporary server listens only on the socket; TCP means the final server.
    until docker exec "$name" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1; do
        attempts=$((attempts - 1))
        if [ "$attempts" -eq 0 ]; then
            docker logs "$name" >&2
            echo "PostgreSQL did not become ready in $name" >&2
            return 1
        fi
        sleep 1
    done
}

sql() {
    docker exec -e PGPASSWORD=postgres "$prefix-$1" \
        psql -h 127.0.0.1 -U postgres -Atq -v ON_ERROR_STOP=1 -c "$2"
}

expect() {
    actual=$(sql "$1" "$2")
    test "$actual" = "$3" || {
        echo "$1: $2 returned '$actual', expected '$3'" >&2
        exit 1
    }
}

start default "$image"
expect default 'SHOW archive_mode' on
expect default 'SHOW archive_command' 'walg-wal-push %p'

# User flags come first; the entrypoint's archive flags are appended and win.
start off -e TOPUP_WAL_ARCHIVE=off "$image" postgres -c archive_mode=on \
    -c "archive_command=walg-wal-push %p"
expect off 'SHOW archive_mode' off
sql off 'CREATE TABLE archive_probe (id int); INSERT INTO archive_probe VALUES (1)' >/dev/null
sql off 'SELECT pg_switch_wal()' >/dev/null
sql off 'CHECKPOINT' >/dev/null
test "$(sql off 'SELECT count(*) FROM pg_ls_archive_statusdir()')" -eq 0 || {
    echo "archive status files exist although TOPUP_WAL_ARCHIVE=off" >&2
    exit 1
}

set +e
docker run --rm -e POSTGRES_PASSWORD=postgres -e TOPUP_WAL_ARCHIVE=maybe "$image" \
    >/dev/null 2>&1
status=$?
set -e
test "$status" -eq 64 || {
    echo "invalid TOPUP_WAL_ARCHIVE returned $status instead of 64" >&2
    exit 1
}

echo "TOPUP_WAL_ARCHIVE switch tests passed"
