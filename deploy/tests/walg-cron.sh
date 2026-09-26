#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
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
cat > "$tmp/bin/wal-g" <<'FAKE'
#!/bin/sh
set -eu
printf '%s\n' "$*" >> "$WALG_TEST_CALL"
if [ "$1" = wal-push ] && [ -n "${WALG_TEST_FAIL_PUSH:-}" ]; then
    exit 1
fi
FAKE
chmod +x "$tmp/bin/wal-g"
touch "$tmp/segment"
# archive_command runs wal-push, then refreshes the marker.
PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    AWS_ACCESS_KEY_ID=test \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment"
grep -Fx "wal-push $tmp/segment" "$tmp/wal-g.call" >/dev/null
grep -E '^[0-9]+$' "$tmp/marker/last-success" >/dev/null
[ "$(stat -c %a "$tmp/marker/last-success")" = 644 ]

# A failed upload must leave the marker untouched.
rm "$tmp/marker/last-success"
if PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    WALG_TEST_FAIL_PUSH=1 \
    AWS_ACCESS_KEY_ID=test \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment" 2>/dev/null; then
    echo "failed WAL upload unexpectedly succeeded" >&2
    exit 1
fi
[ ! -e "$tmp/marker/last-success" ]

# Unsealed (no S3 key): archive_command fails at once, without calling WAL-G.
: >"$tmp/wal-g.call"
if PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    AWS_ACCESS_KEY_ID= \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment" 2>/dev/null; then
    echo "WAL archiving without S3 credentials unexpectedly succeeded" >&2
    exit 1
fi
[ ! -s "$tmp/wal-g.call" ] && [ ! -e "$tmp/marker/last-success" ]

# A restored instance neither archives nor pushes base backups into the prefix it restores from.
output=$(
    WALG_CRON_DRY_RUN=1 TOPUP_RESTORE_FROM_BACKUP=on \
        "$root/deploy/scripts/walg-cron" backup-push "0 3 * * *"
)
printf '%s\n' "$output" | grep -Fx 'base backups are disabled while TOPUP_RESTORE_FROM_BACKUP=on' \
    >/dev/null
if printf '%s\n' "$output" | grep -F 'dry-run:' >/dev/null; then
    echo "walg-cron scheduled a base backup while TOPUP_RESTORE_FROM_BACKUP=on" >&2
    exit 1
fi
: >"$tmp/wal-g.call"
if PATH="$root/deploy/scripts:$tmp/bin:$PATH" \
    WALG_TEST_CALL="$tmp/wal-g.call" \
    TOPUP_RESTORE_FROM_BACKUP=on \
    AWS_ACCESS_KEY_ID=test \
    TOPUP_BACKUP_TIMESTAMP_FILE="$tmp/marker/last-success" \
    "$root/deploy/scripts/walg-cron" wal-push "$tmp/segment" 2>/dev/null; then
    echo "WAL archiving while TOPUP_RESTORE_FROM_BACKUP=on unexpectedly succeeded" >&2
    exit 1
fi
[ ! -s "$tmp/wal-g.call" ] && [ ! -e "$tmp/marker/last-success" ]

# walg-timeline-backup: a base backup is taken at start only when none is on the current timeline.
mkdir -p "$tmp/timeline-bin"
cat >"$tmp/timeline-bin/psql" <<'FAKE'
#!/bin/sh
printf '%s\n' "$TEST_TIMELINE"
FAKE
cat >"$tmp/timeline-bin/wal-g" <<'FAKE'
#!/bin/sh
set -eu
case "$*" in
    "backup-list --json") printf '%s\n' "$TEST_BACKUP_LIST" ;;
    "backup-push "*) printf '%s\n' "$2" >>"$TEST_BASE_BACKUP_CALL" ;;
    *) exit 70 ;;
esac
FAKE
chmod +x "$tmp/timeline-bin/psql" "$tmp/timeline-bin/wal-g"
timeline_backup() {
    : >"$tmp/base-backup.call"
    set +e
    PATH="$tmp/timeline-bin:$PATH" \
        WALG_BIN="$tmp/timeline-bin/wal-g" \
        TEST_BASE_BACKUP_CALL="$tmp/base-backup.call" \
        TEST_TIMELINE="$1" \
        TEST_BACKUP_LIST="$2" \
        AWS_ACCESS_KEY_ID="$3" \
        "$root/deploy/scripts/walg-timeline-backup" /var/lib/postgresql/data >/dev/null 2>&1
    timeline_status=$?
    set -e
}
listed='[{"backup_name":"base_000000010000000000000003","time":"2026-09-22T03:00:00Z"}]'
timeline_backup 1 "$listed" test
[ "$timeline_status" -eq 3 ] && [ ! -s "$tmp/base-backup.call" ] || {
    echo "walg-timeline-backup took a backup although one covers timeline 1" >&2
    exit 1
}
timeline_backup 2 "$listed" test
[ "$timeline_status" -eq 0 ] && grep -Fx /var/lib/postgresql/data "$tmp/base-backup.call" >/dev/null || {
    echo "walg-timeline-backup did not back up the uncovered timeline 2" >&2
    exit 1
}
timeline_backup 1 '[]' test
[ "$timeline_status" -eq 0 ] && [ -s "$tmp/base-backup.call" ] || {
    echo "walg-timeline-backup did not back up a prefix without base backups" >&2
    exit 1
}
timeline_backup 2 "$listed" ''
[ "$timeline_status" -eq 1 ] && [ ! -s "$tmp/base-backup.call" ] || {
    echo "walg-timeline-backup ran without object-storage credentials" >&2
    exit 1
}

echo "walg-cron dry-run test passed"
