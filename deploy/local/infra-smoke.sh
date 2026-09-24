#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/docker-compose.yml"
local_compose="$root/deploy/local/docker-compose.yml"
project="topup-infra-smoke-$$"
tmp=$(mktemp -d "$root/.infra-smoke.XXXXXX")
topup_container=
export TOPUP_LOCAL_DSTACK_IMAGE="$project-dstack"
export TOPUP_LOCAL_POSTGRES_IMAGE="$project-postgres"
export TOPUP_LOCAL_SERVICE_IMAGE="$project-topup"

dc() {
    docker compose -p "$project" -f "$compose" -f "$local_compose" "$@"
}

cleanup() {
    if [ -n "$topup_container" ]; then
        docker rm -f "$topup_container" >/dev/null 2>&1 || true
    fi
    dc down --volumes --remove-orphans
    docker image rm "$TOPUP_LOCAL_DSTACK_IMAGE" "$TOPUP_LOCAL_POSTGRES_IMAGE" \
        "$TOPUP_LOCAL_SERVICE_IMAGE" >/dev/null 2>&1 || true
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

wait_for() {
    description=$1
    shift
    attempts=60
    while [ "$attempts" -gt 0 ]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 2
    done
    echo "timed out waiting for $description" >&2
    return 1
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}

dc build postgres dstack-simulator topup
dc up -d postgres dstack-simulator backup
wait_for postgres dc exec -T postgres pg_isready -U postgres -d topup
wait_for dstack-simulator dc exec -T dstack-simulator test -S /var/run/dstack.sock
wait_for backup dc exec -T backup wal-g --version
echo "local backup service is running"

# The backup key and the database credential files, all derived by `keys`, one volume each.
key_metadata=$(dc exec -T postgres stat -c '%n:%a:%u:%g' /run/wal-g/backup.key \
    /run/db-owner/postgres.password /run/db-owner/postgres.pgpass /run/db-app/topup_service.pgpass)
test "$key_metadata" = "/run/wal-g/backup.key:600:999:999
/run/db-owner/postgres.password:600:999:999
/run/db-owner/postgres.pgpass:600:999:999
/run/db-app/topup_service.pgpass:600:999:999"
if docker inspect "${project}-postgres-1" --format '{{range .Config.Env}}{{println .}}{{end}}' |
    grep -Eq '^(WALG_LIBSODIUM_KEY|POSTGRES_PASSWORD|PGPASSWORD)='; then
    echo "key and password values must not be present in the container environment" >&2
    exit 1
fi
echo "key tmpfs permissions passed"

dc run --rm migrate
dc run --rm --no-deps topup \
    topup route validate --template /etc/topup/routes/phala-cloud-sepolia-pha.yaml

attestation=$(dc run --rm --no-deps topup \
    topup attest --nonce deadbeef)
printf '%s\n' "$attestation" | jq -e \
    '.keyid == "settlement/v1" and (.settlement_pubkey | length == 64) and (.quote | length > 0)' \
    >/dev/null
echo "dstack simulator attestation passed"

settings=$(dc exec -T postgres \
    psql -U postgres -d topup -At -c \
    "select name || '=' || setting from pg_settings where name in ('archive_command','archive_mode','archive_timeout') order by name")
printf '%s\n' "$settings"
printf '%s\n' "$settings" | grep -Fx 'archive_command=walg-cron wal-push %p' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_mode=on' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_timeout=60' >/dev/null
echo "postgres archive settings passed"

# No manual WAL switch: the heartbeat's one row per minute must refresh the marker on an otherwise
# idle database.
dc up -d heartbeat
wait_for backup-marker dc exec -T postgres test -s /run/topup-observability/last-backup-unix-seconds
marker_age() {
    dc exec -T postgres sh -c \
        'echo $(( $(date -u +%s) - $(cat /run/topup-observability/last-backup-unix-seconds) ))'
}
idle_checks=16
while [ "$idle_checks" -gt 0 ]; do
    age=$(marker_age)
    # Worst case for an idle database is about two archive_timeout periods plus the upload; see
    # the idle-database note in deploy/runbooks/backup-age.md.
    if [ "$age" -gt 150 ]; then
        echo "idle backup marker is ${age}s old; beyond the idle worst case" >&2
        exit 1
    fi
    idle_checks=$((idle_checks - 1))
    sleep 10
done
echo "idle database kept the backup marker fresh for 160s"
marker_mode=$(dc exec -T postgres \
    stat -c %a /run/topup-observability/last-backup-unix-seconds)
[ "$marker_mode" = 644 ]

topup_container=$(docker create "$TOPUP_LOCAL_SERVICE_IMAGE")
docker cp "$topup_container:/etc/passwd" "$tmp/passwd"
docker rm "$topup_container" >/dev/null
topup_container=
topup_uid=$(awk -F: '$1 == "nonroot" { print $3 }' "$tmp/passwd")
[ -n "$topup_uid" ]
docker run --rm --user "$topup_uid" \
    --volume "${project}_observability:/run/topup-observability:ro" \
    --entrypoint /bin/sh "$TOPUP_LOCAL_POSTGRES_IMAGE" \
    -c 'test -r /run/topup-observability/last-backup-unix-seconds && read -r timestamp < /run/topup-observability/last-backup-unix-seconds && test -n "$timestamp"'
echo "backup marker is readable by the topup image UID"

dry_run=$(dc run --rm --no-deps \
    -e WALG_CRON_DRY_RUN=1 backup walg-cron backup-push "0 3 * * *")
printf '%s\n' "$dry_run"
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: walg-base-backup /var/lib/postgresql/data' >/dev/null
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: wal-g delete retain FULL 2 --use-sentinel-time --confirm' >/dev/null
echo "backup helper dry-run passed"

echo "infra smoke passed"
