#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cat >"$tmp/wal-g" <<'EOF_FAKE'
#!/bin/sh
printf '%s\n' "$*" >>"$FAKE_LOG"
exit "$FAKE_WAL_FETCH_STATUS"
EOF_FAKE
chmod +x "$tmp/wal-g"
export WALG_BIN="$tmp/wal-g" FAKE_LOG="$tmp/wal-g.log"
wal=000000010000000000000002

# restore_command: a fetched segment succeeds, WAL-G's 74 (not archived) ends recovery with 1, and
# any other failure (storage, decryption) is 126, which aborts recovery instead of promoting.
expect() {
    set +e
    FAKE_WAL_FETCH_STATUS=$1 "$root/deploy/scripts/walg-restore-command" "$wal" "$tmp/destination"
    status=$?
    set -e
    test "$status" -eq "$2" || {
        echo "wal-fetch status $1 returned $status, expected $2" >&2
        exit 1
    }
}
expect 0 0
expect 74 1
expect 1 126
expect 2 126
grep -Fx "wal-fetch $wal $tmp/destination" "$tmp/wal-g.log" >/dev/null

echo "WAL-G restore-command tests passed"
