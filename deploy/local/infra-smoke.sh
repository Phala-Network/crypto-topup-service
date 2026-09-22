#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
project="topup-infra-smoke-$$"

cleanup() {
    docker compose -p "$project" -f "$compose" down --volumes --remove-orphans
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
printf '%s\n' "$settings" | grep -Fx 'archive_command=wal-g wal-push %p' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_mode=on' >/dev/null
printf '%s\n' "$settings" | grep -Fx 'archive_timeout=60' >/dev/null
echo "postgres archive settings passed"

dry_run=$(docker compose -p "$project" -f "$compose" run --rm --no-deps \
    -e WALG_CRON_DRY_RUN=1 backup walg-cron backup-push "0 3 * * *")
printf '%s\n' "$dry_run"
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: wal-g backup-push /var/lib/postgresql/data' >/dev/null
printf '%s\n' "$dry_run" | grep -F \
    'dry-run: wal-g delete retain FULL 2 --use-sentinel-time --confirm' >/dev/null
echo "backup helper dry-run passed"

echo "infra smoke passed"
