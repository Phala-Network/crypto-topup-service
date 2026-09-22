#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

touch "$tmp/alpha" "$tmp/beta"
output=$(
    cd "$tmp"
    WALG_CRON_DRY_RUN=1 \
        WALG_RETENTION_FULL=3 \
        PGDATA=/var/lib/postgresql/data \
        "$root/deploy/scripts/walg-cron" backup-push "0 3 * * *"
)

printf '%s\n' "$output"
printf '%s\n' "$output" | grep -F \
    'parsed schedule: minute=0 hour=3 day-of-month=* month=* day-of-week=*' >/dev/null
printf '%s\n' "$output" | grep -F 'next WAL-G base backup at ' >/dev/null
printf '%s\n' "$output" | grep -F \
    'dry-run: walg-base-backup /var/lib/postgresql/data' >/dev/null
printf '%s\n' "$output" | grep -F \
    'dry-run: wal-g delete retain FULL 3 --use-sentinel-time --confirm' >/dev/null

echo "walg-cron dry-run test passed"
