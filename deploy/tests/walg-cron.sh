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

mkdir -p "$tmp/bin" "$tmp/marker" "$tmp/keys"
cat > "$tmp/bin/wal-g" <<'FAKE'
#!/bin/sh
set -eu
printf 'KEY=%s ARGS=%s\n' "${WALG_LIBSODIUM_KEY_PATH:-}" "$*" >> "$WALG_TEST_CALL"
if [ "$1" = wal-push ] && [ -n "${WALG_TEST_FAIL_PUSH:-}" ]; then
    exit 1
fi
FAKE
chmod +x "$tmp/bin/wal-g"
touch "$tmp/segment" "$tmp/keys/backup-v1.key"
# archive_command delegates to the key-versioned walg-wal-push, then refreshes the marker.
PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    WALG_KEY_DIR="$tmp/keys" \
    TOPUP_BACKUP_KEY_VERSION=1 \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment"
grep -F "KEY=$tmp/keys/backup-v1.key ARGS=wal-push $tmp/segment" "$tmp/wal-g.call" >/dev/null
grep -F "key-versions/wal/segment.json" "$tmp/wal-g.call" >/dev/null
grep -E '^[0-9]+$' "$tmp/marker/last-success" >/dev/null
[ "$(stat -c %a "$tmp/marker/last-success")" = 644 ]

# A failed upload must leave the marker untouched.
rm "$tmp/marker/last-success"
if PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    WALG_TEST_FAIL_PUSH=1 \
    WALG_KEY_DIR="$tmp/keys" \
    TOPUP_BACKUP_KEY_VERSION=1 \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment" 2>/dev/null; then
    echo "failed WAL upload unexpectedly succeeded" >&2
    exit 1
fi
[ ! -e "$tmp/marker/last-success" ]

echo "walg-cron dry-run test passed"
