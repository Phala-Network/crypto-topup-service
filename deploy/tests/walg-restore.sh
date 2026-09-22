#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

mkdir -p "$tmp/bin" "$tmp/store/key-versions/base" "$tmp/store/key-versions/wal" "$tmp/keys"
cat >"$tmp/bin/wal-g" <<'EOF'
#!/bin/sh
set -eu
printf 'KEY=%s ARGS=%s\n' "${WALG_LIBSODIUM_KEY_PATH:-}" "$*" >>"$FAKE_LOG"
case "$1:$2" in
    st:put)
        shift 2
        while [ "${1#-}" != "$1" ]; do shift; done
        source=$1
        destination=$2
        mkdir -p "$FAKE_STORE/$(dirname "$destination")"
        cp "$source" "$FAKE_STORE/$destination"
        ;;
    st:get)
        source=$3
        destination=$4
        cp "$FAKE_STORE/$source" "$destination"
        ;;
    st:check) exit "${FAKE_ST_CHECK_STATUS:-0}" ;;
    backup-push:*) ;;
    backup-list:*)
        printf '[{"backup_name":"base_000000010000000000000001","start_time":"2026-09-22T00:00:00Z"}]\n'
        ;;
    backup-fetch:*) mkdir -p "$2"; printf '16\n' >"$2/PG_VERSION" ;;
    wal-fetch:*) exit "${FAKE_WAL_FETCH_STATUS:-0}" ;;
    *) echo "unexpected fake wal-g invocation: $*" >&2; exit 70 ;;
esac
EOF
chmod +x "$tmp/bin/wal-g"

export PATH="$root/deploy/scripts:$tmp/bin:$PATH"
export WALG_BIN="$tmp/bin/wal-g"
export FAKE_LOG="$tmp/wal-g.log"
export FAKE_STORE="$tmp/store"
export TOPUP_BACKUP_KEY_VERSION=1
export WALG_KEY_DIR="$tmp/keys"
touch "$tmp/keys/backup-v1.key"

backup_name=$(walg-base-backup "$tmp/pgdata")
test "$backup_name" = base_000000010000000000000001
jq -e '.key_version == 1 and .kind == "base"' \
    "$tmp/store/key-versions/base/$backup_name.json" >/dev/null
jq -e '.key_version == 1' "$tmp/store/key-versions/current.json" >/dev/null

walg-backup-fetch "$tmp/restored" "$backup_name"
grep -F "KEY=$tmp/keys/backup-v1.key ARGS=backup-fetch $tmp/restored $backup_name" \
    "$tmp/wal-g.log" >/dev/null

wal=000000010000000000000002
walg-key-manifest wal "$wal"
FAKE_WAL_FETCH_STATUS=0 walg-restore-command "$wal" "$tmp/wal"
grep -F "KEY=$tmp/keys/backup-v1.key ARGS=wal-fetch $wal $tmp/wal" \
    "$tmp/wal-g.log" >/dev/null

if FAKE_WAL_FETCH_STATUS=74 walg-restore-command "$wal" "$tmp/missing"; then
    echo "missing WAL unexpectedly succeeded" >&2
    exit 1
else
    test "$?" -eq 1
fi
if FAKE_WAL_FETCH_STATUS=2 walg-restore-command "$wal" "$tmp/broken"; then
    echo "WAL storage/decryption failure unexpectedly succeeded" >&2
    exit 1
else
    test "$?" -eq 126
fi

rm -f "$tmp/store/key-versions/wal/$wal.json" "$tmp/store/key-versions/current.json"
if FAKE_ST_CHECK_STATUS=2 walg-restore-command "$wal" "$tmp/unavailable"; then
    echo "metadata storage failure unexpectedly succeeded" >&2
    exit 1
else
    test "$?" -eq 126
fi

echo "WAL-G key manifest and restore-command tests passed"
