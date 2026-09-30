#!/bin/sh
# Starts the postgres-walg image and proves its bootstrap and archive switch: an empty data
# directory is initialized only when the backup prefix is listed and holds no base backup, and
# never after a listing error; a malformed object-store setting stops it; the service archives;
# TOPUP_RESTORE_FROM_BACKUP=on (the restore-check variant) forces archiving off even against
# user-supplied flags, requires a base backup, and never touches a data directory that holds
# anything. WAL-G's file storage stands in for object storage. deploy/local/restore-drill.sh runs
# the restore itself end to end.
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
prefix="topup-archive-switch-$$"
containers=
volume="$prefix-data"
built_image=

cleanup() {
    for container in $containers; do
        docker rm -f "$container" >/dev/null 2>&1 || true
    done
    docker volume rm "$volume" >/dev/null 2>&1 || true
    if [ -n "$built_image" ]; then
        docker image rm "$built_image" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# Test the given image (CI passes the one it just built); otherwise build a per-run tag from this
# checkout, because a shared tag such as phala-pay-postgres-walg:local may be stale.
if [ "$#" -ge 1 ]; then
    image=$1
else
    built_image="phala-pay-postgres-walg:$prefix"
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

# An empty prefix: WAL-G lists no base backup, so a new cluster is initialized, and it archives.
start default -e WALG_FILE_PREFIX=/tmp "$image"
expect default 'SHOW archive_mode' on
expect default 'SHOW archive_command' 'walg-cron wal-push %p'
expect default 'SHOW restore_command' 'walg-restore-command %f %p'

# expect_exit STATUS MESSAGE ARGS...: the container exits with STATUS and logs MESSAGE.
expect_exit() {
    expected=$1
    message=$2
    shift 2
    set +e
    output=$(docker run --rm -e POSTGRES_PASSWORD=postgres "$@" 2>&1)
    status=$?
    set -e
    if [ "$status" -ne "$expected" ] || ! printf '%s\n' "$output" | grep -F -- "$message" >/dev/null; then
        printf '%s\n' "$output" >&2
        echo "expected exit $expected with '$message', got $status" >&2
        exit 1
    fi
}

expect_exit 64 "TOPUP_RESTORE_FROM_BACKUP must be on or off" -e WALG_FILE_PREFIX=/tmp \
    -e TOPUP_RESTORE_FROM_BACKUP=maybe "$image"
# PostgreSQL and the backup job start only with WAL-G's file backend or all three S3 settings well
# formed (the Phala Cloud template takes them from its deploy form); other commands need none.
# expect_store STATUS MESSAGE PREFIX ENDPOINT REGION ARGS...: expect_exit with those S3 settings.
expect_store() {
    store_status=$1 store_message=$2 store_prefix=$3 store_endpoint=$4 store_region=$5
    shift 5
    expect_exit "$store_status" "$store_message" -e "WALG_S3_PREFIX=$store_prefix" \
        -e "AWS_ENDPOINT=$store_endpoint" -e "AWS_REGION=$store_region" "$@"
}
store_bucket=s3://topup-backups/postgres
expect_exit 64 "WALG_S3_PREFIX must be s3://BUCKET[/PATH]" "$image"
expect_store 64 "WALG_S3_PREFIX must be s3://BUCKET[/PATH]" s3://a..b/p https://s3.example auto "$image"
expect_store 64 "AWS_REGION must be a region name" "$store_bucket" https://s3.example "" "$image"
expect_store 64 "set WALG_FILE_PREFIX or WALG_S3_PREFIX, not both" "$store_bucket" https://s3.example auto \
    -e WALG_FILE_PREFIX=/tmp "$image"
# AWS_ENDPOINT, parsed as a URL: an https origin with a DNS name or an IP address and a valid port.
# An accepted one reaches walg-cron, which refuses its missing arguments.
for endpoint in https://S3.EXAMPLE https://s3.example:443/ 'https://[2001:db8::1]:9000' https://192.0.2.1; do
    expect_store 64 "usage: walg-cron" "$store_bucket" "$endpoint" auto "$image" walg-cron
done
for endpoint in "" s3.example http://s3.example https://999.999.999.999 https://1.2.3 'https://[zz::1]' \
    https://a..b https://-a.example https://s3.example/path 'https://s3.example?x=1' \
    'https://s3.example#f' https://user@s3.example https://s3.example:0 https://s3.example:99999; do
    expect_store 64 "AWS_ENDPOINT must be an https origin" "$store_bucket" "$endpoint" auto "$image" walg-cron
done
# Plain http only with the local stacks' switch, which no attested compose can carry.
expect_store 64 "usage: walg-cron" "$store_bucket" http://s3:3900 us-east-1 -e TOPUP_OBJECT_STORE_ALLOW_HTTP=on \
    "$image" walg-cron

# A listing error (here: a prefix that does not exist) must fail, not fall back to initdb, in
# either variant; so must the restore-check variant with nothing to restore.
expect_exit 1 "refusing to initialize an empty data directory" \
    -v "$volume:/var/lib/postgresql" -e WALG_FILE_PREFIX=/nonexistent "$image"
expect_exit 1 "refusing to initialize an empty data directory" \
    -v "$volume:/var/lib/postgresql" -e WALG_FILE_PREFIX=/nonexistent \
    -e TOPUP_RESTORE_FROM_BACKUP=on "$image"
expect_exit 1 "the backup prefix holds no base backup" \
    -v "$volume:/var/lib/postgresql" -e WALG_FILE_PREFIX=/tmp -e TOPUP_RESTORE_FROM_BACKUP=on "$image"
test -z "$(docker run --rm -v "$volume:/var/lib/postgresql" --entrypoint ls "$image" \
    -A /var/lib/postgresql/data)" || {
    echo "a failed bootstrap left files in the data directory" >&2
    exit 1
}

# A data directory that holds a cluster is started as is, without listing the prefix: no fetch,
# and with TOPUP_RESTORE_FROM_BACKUP=on archiving off even when a user flag asks for it.
start existing -v "$volume:/var/lib/postgresql" -e WALG_FILE_PREFIX=/tmp "$image"
sql existing 'CREATE TABLE existing_probe (id int); INSERT INTO existing_probe VALUES (1)' >/dev/null
docker rm -f "$prefix-existing" >/dev/null
start restored -v "$volume:/var/lib/postgresql" -e WALG_FILE_PREFIX=/nonexistent \
    -e TOPUP_RESTORE_FROM_BACKUP=on "$image" postgres -c archive_mode=on \
    -c "archive_command=walg-cron wal-push %p"
expect restored 'SHOW archive_mode' off
expect restored 'SHOW restore_command' 'walg-restore-command %f %p'
expect restored 'SELECT count(*) FROM existing_probe' 1
sql restored 'SELECT pg_switch_wal()' >/dev/null
sql restored 'CHECKPOINT' >/dev/null
test "$(sql restored 'SELECT count(*) FROM pg_ls_archive_statusdir()')" -eq 0 || {
    echo "archive status files exist although TOPUP_RESTORE_FROM_BACKUP=on" >&2
    exit 1
}

echo "postgres-walg bootstrap and archive switch tests passed"
