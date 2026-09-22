#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
project="topup-restore-drill-$$"

dc() {
    docker compose -p "$project" -f "$compose" "$@"
}

cleanup() {
    dc --profile tools down --volumes --remove-orphans >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

wait_for() {
    description=$1
    shift
    attempts=90
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

heartbeat_ready() {
    test "$(psql_value 'SELECT count(*) > 0 FROM heartbeat' 2>/dev/null)" = t
}

archive_advanced() {
    test "$(psql_value "SELECT archived_count > $archived_before FROM pg_stat_archiver")" = t
}

recovery_promoted() {
    test "$(psql_value 'SELECT NOT pg_is_in_recovery()')" = t
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}

dc build postgres dstack-simulator topup
dc up -d postgres backup heartbeat
wait_for postgres dc exec -T postgres pg_isready -U postgres -d topup
wait_for heartbeat heartbeat_ready

before_id=$(psql_value 'INSERT INTO heartbeat DEFAULT VALUES RETURNING id')
dc exec -T backup wal-g backup-push /var/lib/postgresql/data

dc stop heartbeat >/dev/null
after_record=$(psql_value \
    "INSERT INTO heartbeat DEFAULT VALUES RETURNING id || '|' || extract(epoch FROM recorded_at)::bigint")
after_id=${after_record%%|*}
source_epoch=${after_record#*|}
target_lsn=$(psql_value 'SELECT pg_current_wal_lsn()')
archived_before=$(psql_value 'SELECT archived_count FROM pg_stat_archiver')
psql_value 'SELECT pg_switch_wal()' >/dev/null
wait_for "forced WAL archive" archive_advanced

rto_started=$(date +%s)
dc stop backup postgres >/dev/null
dc rm -f backup postgres heartbeat migrate >/dev/null
pg_volume=$(docker volume ls -q \
    --filter "label=com.docker.compose.project=$project" \
    --filter "label=com.docker.compose.volume=pgdata")
if [ -z "$pg_volume" ]; then
    echo "could not locate drill PostgreSQL volume" >&2
    exit 1
fi
docker volume rm "$pg_volume" >/dev/null

dc run --rm --no-deps -e RECOVERY_TARGET_LSN="$target_lsn" restore '
    rm -rf "$PGDATA"/*
    wal-g backup-fetch "$PGDATA" LATEST
    cat >>"$PGDATA/postgresql.auto.conf" <<EOF
restore_command = '\''wal-g wal-fetch %f %p'\''
recovery_target_lsn = '\''$RECOVERY_TARGET_LSN'\''
recovery_target_inclusive = true
recovery_target_action = '\''promote'\''
EOF
    touch "$PGDATA/recovery.signal"
    chmod 0700 "$PGDATA"
'

dc up -d postgres
wait_for "restored PostgreSQL" dc exec -T postgres pg_isready -U postgres -d topup
wait_for "archive recovery promotion" recovery_promoted

restore_report=$(dc run --rm --no-deps migrate topup restore-check)
restored_rows=$(psql_value \
    "SELECT count(*) FROM heartbeat WHERE id IN ($before_id, $after_id)")
if [ "$restored_rows" -ne 2 ]; then
    echo "latest drill rows were not restored" >&2
    exit 1
fi
restored_epoch=$(psql_value 'SELECT extract(epoch FROM max(recorded_at))::bigint FROM heartbeat')
measured_rpo=$((source_epoch - restored_epoch))
if [ "$measured_rpo" -lt 0 ]; then
    measured_rpo=0
fi
if [ "$measured_rpo" -gt 60 ]; then
    echo "measured RPO ${measured_rpo}s exceeds 60s" >&2
    exit 1
fi
rto_elapsed=$(( $(date +%s) - rto_started ))

printf 'restore-check: %s\n' "$restore_report"
printf 'restored_marker_ids=%s,%s\n' "$before_id" "$after_id"
printf 'measured_rpo_seconds=%s\n' "$measured_rpo"
printf 'elapsed_rto_seconds=%s\n' "$rto_elapsed"
echo "restore drill passed"
