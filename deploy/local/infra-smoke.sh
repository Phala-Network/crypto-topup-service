#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
project="topup-infra-smoke-$$"
tmp=$(mktemp -d "$root/.infra-smoke.XXXXXX")
topup_container=
export TOPUP_LOCAL_DSTACK_IMAGE="$project-dstack"
export TOPUP_LOCAL_POSTGRES_IMAGE="$project-postgres"
export TOPUP_LOCAL_SERVICE_IMAGE="$project-topup"

cleanup() {
    if [ -n "$topup_container" ]; then
        docker rm -f "$topup_container" >/dev/null 2>&1 || true
    fi
    docker compose -p "$project" -f "$compose" down --volumes --remove-orphans
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

docker compose -p "$project" -f "$compose" build postgres dstack-simulator topup
docker compose -p "$project" -f "$compose" up -d postgres dstack-simulator backup
wait_for postgres docker compose -p "$project" -f "$compose" \
    exec -T postgres pg_isready -U postgres -d topup
wait_for dstack-simulator docker compose -p "$project" -f "$compose" \
    exec -T dstack-simulator test -S /var/run/dstack.sock
wait_for backup docker compose -p "$project" -f "$compose" \
    exec -T backup wal-g --version
echo "local backup service is running"

key_metadata=$(docker compose -p "$project" -f "$compose" exec -T postgres \
    stat -c '%a:%u:%g' /run/wal-g/backup.key)
test "$key_metadata" = "600:999:999"
if docker inspect "${project}-postgres-1" --format '{{range .Config.Env}}{{println .}}{{end}}' |
    grep -q '^WALG_LIBSODIUM_KEY='; then
    echo "backup key value must not be present in the container environment" >&2
    exit 1
fi
echo "backup key tmpfs permissions passed"

docker compose -p "$project" -f "$compose" run --rm migrate
docker compose -p "$project" -f "$compose" run --rm --no-deps topup \
    topup route validate --template /etc/topup/routes/phala-cloud-sepolia-pha.yaml

attestation=$(docker compose -p "$project" -f "$compose" run --rm --no-deps topup \
    topup attest --nonce deadbeef)
printf '%s\n' "$attestation" | jq -e \
    '.keyid == "settlement/v1" and (.settlement_pubkey | length == 64) and (.quote | length > 0)' \
    >/dev/null
echo "dstack simulator attestation passed"

settings=$(docker compose -p "$project" -f "$compose" exec -T postgres \
    psql -U postgres -d topup -At -c \
    "select name || '=' || setting from pg_settings where name in ('archive_command','archive_mode','archive_timeout') order by name")
printf '%s\n' "$settings"
printf '%s\n' "$settings" | grep -Fx 'archive_command=walg-cron wal-push %p' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_mode=on' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_timeout=60' >/dev/null
echo "postgres archive settings passed"

# No manual WAL switch: the backup keepalive must refresh the marker on an idle database.
wait_for backup-marker docker compose -p "$project" -f "$compose" \
    exec -T postgres test -s /run/topup-observability/last-backup-unix-seconds
marker_age() {
    docker compose -p "$project" -f "$compose" exec -T postgres sh -c \
        'echo $(( $(date -u +%s) - $(cat /run/topup-observability/last-backup-unix-seconds) ))'
}
idle_checks=16
while [ "$idle_checks" -gt 0 ]; do
    age=$(marker_age)
    if [ "$age" -gt 120 ]; then
        echo "idle backup marker is ${age}s old; TopupBackupTooOld would fire" >&2
        exit 1
    fi
    idle_checks=$((idle_checks - 1))
    sleep 10
done
echo "idle database kept the backup marker fresh for 160s"
marker_mode=$(docker compose -p "$project" -f "$compose" exec -T postgres \
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

dry_run=$(docker compose -p "$project" -f "$compose" run --rm --no-deps \
    -e WALG_CRON_DRY_RUN=1 backup walg-cron backup-push "0 3 * * *")
printf '%s\n' "$dry_run"
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: walg-base-backup /var/lib/postgresql/data' >/dev/null
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: wal-g delete retain FULL 2 --use-sentinel-time --confirm' >/dev/null
echo "backup helper dry-run passed"

echo "infra smoke passed"
