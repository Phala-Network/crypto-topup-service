#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
mode=${1:-all}

case "$mode" in
    all)
        "$0" controlled
        "$0" crash
        exit 0
        ;;
    controlled|crash) ;;
    *) echo "usage: $0 [controlled|crash|all]" >&2; exit 64 ;;
esac

project="topup-restore-drill-$mode-$$"
writer_pid=
samples_file=
dc() {
    docker compose -p "$project" -f "$compose" "$@"
}

cleanup() {
    if [ -n "$writer_pid" ]; then
        kill "$writer_pid" >/dev/null 2>&1 || true
        wait "$writer_pid" >/dev/null 2>&1 || true
    fi
    if [ -n "$samples_file" ]; then
        rm -f "$samples_file"
    fi
    dc --profile tools down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

wait_for() {
    description=$1
    shift
    attempts=150
    while [ "$attempts" -gt 0 ]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 1
    done
    echo "timed out waiting for $description" >&2
    return 1
}

psql_value() {
    dc exec -T postgres psql -U postgres -d topup -Atq -v ON_ERROR_STOP=1 -c "$1"
}

archive_advanced() {
    test "$(psql_value "SELECT archived_count > $archived_before FROM pg_stat_archiver")" = t
}

recovery_promoted() {
    test "$(psql_value 'SELECT NOT pg_is_in_recovery()')" = t
}

record_sample() {
    psql_value "INSERT INTO heartbeat DEFAULT VALUES; INSERT INTO restore_drill_marker(mode) VALUES ('$mode') RETURNING id" | tail -1
}

seed_reconciliation_fixture() {
    dc exec -T postgres psql -U postgres -d topup -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE restore_drill_marker (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    mode text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

INSERT INTO products (id, slug, settlement_url, webhook_url, pubkey, kid)
VALUES (
    '11111111-1111-1111-1111-111111111111',
    'restore-drill',
    'http://mock-product:8081/settlements',
    'http://mock-product:8081/webhooks',
    'restore-drill-key',
    'product/restore-drill'
);
INSERT INTO accounts (id, product_id, external_id)
VALUES (
    '22222222-2222-2222-2222-222222222222',
    '11111111-1111-1111-1111-111111111111',
    'restore-drill-account'
);
INSERT INTO addresses (
    id, account_id, chain_id, kind, version, salt, address
)
VALUES (
    '33333333-3333-3333-3333-333333333333',
    '22222222-2222-2222-2222-222222222222',
    1,
    'persistent',
    1,
    '0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
    '0xdddddddddddddddddddddddddddddddddddddddd'
);
INSERT INTO deposits (
    id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
    address_id, account_id, route, route_version, asset_contract, from_address,
    amount_atomic, state, next_attempt_at, valuation_at, price_scaled, price_source,
    credit_minor
)
VALUES (
    '44444444-4444-4444-4444-444444444444',
    1,
    '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    0,
    100,
    '0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    '2026-09-22T00:00:00Z',
    '33333333-3333-3333-3333-333333333333',
    '22222222-2222-2222-2222-222222222222',
    'restore-drill',
    1,
    '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
    '0xcccccccccccccccccccccccccccccccccccccccc',
    1000,
    'cleared',
    now(),
    '2026-09-22T00:00:00Z',
    25000000,
    'spot',
    250
);
INSERT INTO settlements (deposit_id, product_id, key, payload, status, sent_at)
VALUES (
    '44444444-4444-4444-4444-444444444444',
    '11111111-1111-1111-1111-111111111111',
    'deposit:44444444-4444-4444-4444-444444444444',
    jsonb_build_object(
        'version', 1,
        'idempotency_key', 'deposit:44444444-4444-4444-4444-444444444444',
        'account_id', 'restore-drill-account',
        'unit', 'USD',
        'amount_minor', '250',
        'source', 'crypto_deposit',
        'evidence', jsonb_build_object(
            'chain_id', 1,
            'asset_contract', '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
            'route', 'restore-drill',
            'route_version', 1,
            'tx_hash', '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
            'log_index', 0,
            'to', '0xdddddddddddddddddddddddddddddddddddddddd',
            'amount_atomic', '1000',
            'price_scaled', '25000000',
            'price_scale', 8,
            'valuation_at', '2026-09-22T00:00:00Z',
            'lock_ref', NULL
        )
    ),
    'sent',
    now()
);
INSERT INTO heartbeat DEFAULT VALUES;
INSERT INTO restore_drill_marker(mode) VALUES ('base');
SQL
}

test_restore_failures_are_fatal() {
    wal_name=$1
    dc exec -T -e TOPUP_BACKUP_KEY_VERSION=0 backup walg-key-manifest wal "$wal_name" >/dev/null
    set +e
    dc exec -T backup walg-restore-command "$wal_name" /tmp/wrong-key-wal >/dev/null 2>&1
    status=$?
    set -e
    test "$status" -eq 126 || {
        echo "wrong WAL key returned $status instead of 126" >&2
        return 1
    }
    dc exec -T -e TOPUP_BACKUP_KEY_VERSION=1 backup walg-key-manifest wal "$wal_name" >/dev/null
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
export TOPUP_BACKUP_KEY_FALLBACK_VERSIONS=${TOPUP_BACKUP_KEY_FALLBACK_VERSIONS:-0}

dc build postgres dstack-simulator topup
dc up -d postgres backup mock-product
wait_for postgres dc exec -T postgres pg_isready -U postgres -d topup
wait_for mock-product dc exec -T mock-product python3 -c \
    "import urllib.request; urllib.request.urlopen('http://localhost:8081/health')"
dc run --rm migrate >/dev/null
seed_reconciliation_fixture

backup_name=$(dc exec -T backup walg-base-backup /var/lib/postgresql/data | tail -1)
case "$backup_name" in
    base_*) ;;
    *) echo "could not determine WAL-G base backup name" >&2; exit 1 ;;
esac

archived_before=$(psql_value 'SELECT archived_count FROM pg_stat_archiver')
upload_started=$(date +%s)
if [ "$mode" = controlled ]; then
    first_marker=$(record_sample)
    last_marker=$(record_sample)
    psql_value 'SELECT pg_switch_wal()' >/dev/null
else
    samples_file=$(mktemp)
    record_sample >"$samples_file"
    (
        while :; do
            sleep 1
            record_sample >>"$samples_file"
        done
    ) &
    writer_pid=$!
    wait_for "archive_timeout WAL upload" archive_advanced
    sleep 5
    kill "$writer_pid" >/dev/null 2>&1 || true
    wait "$writer_pid" >/dev/null 2>&1 || true
    writer_pid=
    first_marker=$(head -1 "$samples_file")
    last_marker=$(tail -1 "$samples_file")
    rm -f "$samples_file"
    samples_file=
fi
wait_for "archived WAL" archive_advanced
upload_latency=$(( $(date +%s) - upload_started ))

expected_heartbeat_at=$(psql_value \
    "SELECT to_char(max(recorded_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM heartbeat")
expected_lsn=$(psql_value 'SELECT pg_current_wal_lsn()')
expected_marker=$(psql_value 'SELECT max(id) FROM restore_drill_marker')
last_archived_wal=$(psql_value 'SELECT last_archived_wal FROM pg_stat_archiver')
test -n "$last_archived_wal"
test_restore_failures_are_fatal "$last_archived_wal"

rto_started=$(date +%s)
dc stop backup >/dev/null
if [ "$mode" = crash ]; then
    docker kill "${project}-postgres-1" >/dev/null
else
    dc stop postgres >/dev/null
fi
dc rm -f backup postgres heartbeat migrate restore-check >/dev/null 2>&1 || true
pg_volume=$(docker volume ls -q \
    --filter "label=com.docker.compose.project=$project" \
    --filter "label=com.docker.compose.volume=pgdata")
if [ -z "$pg_volume" ]; then
    echo "could not locate drill PostgreSQL volume" >&2
    exit 1
fi
docker volume rm "$pg_volume" >/dev/null

if [ "$mode" = controlled ]; then
    dc run --rm --no-deps \
        -e BACKUP_NAME="$backup_name" \
        -e RECOVERY_TARGET_LSN="$expected_lsn" restore '
        rm -rf "$PGDATA"/*
        walg-backup-fetch "$PGDATA" "$BACKUP_NAME"
        test -s "$PGDATA/PG_VERSION"
        cat >>"$PGDATA/postgresql.auto.conf" <<EOF
restore_command = '\''walg-restore-command %f %p'\''
recovery_target_lsn = '\''$RECOVERY_TARGET_LSN'\''
recovery_target_inclusive = true
recovery_target_action = '\''promote'\''
EOF
        touch "$PGDATA/recovery.signal"
        chmod 0700 "$PGDATA"
    '
else
    dc run --rm --no-deps -e BACKUP_NAME="$backup_name" restore '
        rm -rf "$PGDATA"/*
        walg-backup-fetch "$PGDATA" "$BACKUP_NAME"
        test -s "$PGDATA/PG_VERSION"
        cat >>"$PGDATA/postgresql.auto.conf" <<EOF
restore_command = '\''walg-restore-command %f %p'\''
EOF
        touch "$PGDATA/recovery.signal"
        chmod 0700 "$PGDATA"
    '
fi

dc up -d postgres
wait_for "restored PostgreSQL" dc exec -T postgres pg_isready -U postgres -d topup
wait_for "archive recovery promotion" recovery_promoted

set +e
restore_report=$(dc run --rm --no-deps restore-check \
    topup restore-check \
    --expected-heartbeat-at "$expected_heartbeat_at" \
    --expected-lsn "$expected_lsn")
restore_status=$?
set -e
if [ "$restore_status" -ne 0 ]; then
    printf 'restore-check: %s\n' "$restore_report" >&2
    exit "$restore_status"
fi

measured_rpo=$(printf '%s\n' "$restore_report" | jq -er '.measured_rpo_seconds')
allowed_rpo=$(printf '%s\n' "$restore_report" | jq -er '.allowed_rpo_seconds')
wal_bytes_behind=$(printf '%s\n' "$restore_report" | jq -er '.wal_bytes_behind')
reconciliation=$(printf '%s\n' "$restore_report" | jq -er '.post_restore_reconciliation.status')
restored_marker=$(psql_value 'SELECT max(id) FROM restore_drill_marker')
restored_pricing=$(psql_value \
    "SELECT state || '|' || credit_minor::text || '|' || price_scaled::text FROM deposits WHERE id = '44444444-4444-4444-4444-444444444444'")
rto_elapsed=$(( $(date +%s) - rto_started ))

test "$reconciliation" = complete
test "$restored_pricing" = 'credited|275|27500000'
test "$measured_rpo" -le "$allowed_rpo"
test "$rto_elapsed" -le 3600
if [ "$mode" = controlled ]; then
    test "$restored_marker" -eq "$expected_marker"
    test "$wal_bytes_behind" -eq 0
fi

printf 'mode=%s\n' "$mode"
printf 'restore-check: %s\n' "$restore_report"
printf 'base_backup=%s\n' "$backup_name"
printf 'source_marker_range=%s..%s expected_last=%s restored_last=%s\n' \
    "$first_marker" "$last_marker" "$expected_marker" "$restored_marker"
printf 'measured_rpo_seconds=%s\n' "$measured_rpo"
printf 'allowed_rpo_with_sampling_seconds=%s\n' "$allowed_rpo"
printf 'wal_bytes_behind=%s\n' "$wal_bytes_behind"
printf 'archive_window_seconds=60\n'
printf 'observed_upload_latency_seconds=%s\n' "$upload_latency"
printf 'elapsed_rto_seconds=%s\n' "$rto_elapsed"
echo "restore drill $mode passed"
