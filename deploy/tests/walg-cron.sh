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
    'dry-run: wal-g backup-push /var/lib/postgresql/data' >/dev/null
printf '%s\n' "$output" | grep -F \
    'dry-run: wal-g delete retain FULL 3 --use-sentinel-time --confirm' >/dev/null

mkdir -p "$tmp/bin" "$tmp/marker"
cat > "$tmp/bin/wal-g" <<'EOF'
#!/bin/sh
set -eu
printf '%s\n' "$*" > "$WALG_TEST_CALL"
EOF
chmod +x "$tmp/bin/wal-g"
touch "$tmp/segment"
PATH="$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment"
grep -F "wal-push $tmp/segment" "$tmp/wal-g.call" >/dev/null
grep -E '^[0-9]+$' "$tmp/marker/last-success" >/dev/null

echo "walg-cron dry-run test passed"
