#!/usr/bin/env bash
# bash for pipefail and inherit_errexit: several checks pipe docker or psql output into a filter.
set -euo pipefail
shopt -s inherit_errexit

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
# No bind mounts: CI's Docker daemon cannot see the checkout (see restore-drill.compose.yml).
drill_compose="$root/deploy/local/restore-drill.compose.yml"
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

# TOPUP_RESTORE_DRILL_ID lets a caller (the weekly workflow) find and clean up its own projects.
drill_id=${TOPUP_RESTORE_DRILL_ID:-$$}
case "$drill_id" in
    ''|*[!a-z0-9]*) echo "TOPUP_RESTORE_DRILL_ID must be lowercase alphanumeric" >&2; exit 64 ;;
esac
project="topup-restore-drill-$mode-$drill_id"
# Per-run image tags keep concurrent checkouts from replacing this drill's images mid-run.
export TOPUP_LOCAL_SERVICE_IMAGE="crypto-topup-service:$project"
export TOPUP_LOCAL_POSTGRES_IMAGE="crypto-topup-postgres-walg:$project"
export TOPUP_LOCAL_DSTACK_IMAGE="crypto-topup-dstack-simulator:$project"
writer_pid=
samples_file=
routes_dir=
seed_container="$project-seed"
# The source runs the service variant of the rendered compose; the replacement boots the
# restore-check variant (deploy/RESTORE.md).
variant=()
dc() {
    "$root/deploy/local/compose.sh" "${variant[@]}" -p "$project" -f "$drill_compose" "$@"
}

cleanup() {
    if [ -n "$writer_pid" ]; then
        kill "$writer_pid" >/dev/null 2>&1 || true
        wait "$writer_pid" >/dev/null 2>&1 || true
    fi
    if [ -n "$samples_file" ]; then
        rm -f "$samples_file"
    fi
    docker rm -f "$seed_container" >/dev/null 2>&1 || true
    dc --profile tools down --volumes --remove-orphans >/dev/null 2>&1 || true
    docker image rm "$TOPUP_LOCAL_SERVICE_IMAGE" "$TOPUP_LOCAL_POSTGRES_IMAGE" \
        "$TOPUP_LOCAL_DSTACK_IMAGE" >/dev/null 2>&1 || true
    if [ -n "$routes_dir" ]; then
        rm -rf "$routes_dir"
    fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

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

wait_for_fast() {
    description=$1
    shift
    attempts=750
    while [ "$attempts" -gt 0 ]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 0.2
    done
    echo "timed out waiting for $description" >&2
    return 1
}

psql_value() {
    dc exec -T postgres psql -U postgres -d topup -Atq -v ON_ERROR_STOP=1 -c "$1"
}

# PostgreSQL creates archive_status/<segment>.ready when the segment closes, and the archiver's
# rename to .done keeps that mtime. pg_ls_archive_statusdir() truncates mtime to whole seconds, so
# stat the file itself, and record the time as soon as the file appears: a later checkpoint may
# recycle the segment and remove its .done file.
segment_closed_at=
wal_closed() {
    local closed
    closed=$(dc exec -T postgres sh -c '
        cd "$PGDATA/pg_wal/archive_status"
        stat -c %y "$1.ready" 2>/dev/null || stat -c %y "$1.done"
    ' sh "$1") || return 1
    segment_closed_at=$(date -u -d "$closed" +%s.%3N)
}

wal_manifest_visible() {
    dc exec -T backup wal-g st cat "key-versions/wal/$1.json"
}

wal_object_visible() {
    dc exec -T backup wal-g st ls wal_005/ | grep -F " $1."
}

# Upload time of a WAL object as recorded by object storage, in epoch seconds with milliseconds.
wal_object_uploaded_epoch() {
    line=$(dc exec -T backup wal-g st ls wal_005/ | grep -F " $1.")
    set -- $line
    test "$#" -ge 7 || {
        echo "WAL object listing has no upload time for $1" >&2
        return 1
    }
    date -u -d "$3 $4" +%s.%3N
}

pending_wals() {
    psql_value "SELECT left(name, 24) FROM pg_ls_archive_statusdir() \
        WHERE name ~ '^[0-9A-F]{24}[.]ready$' ORDER BY name"
}

pending_wal_count_at_least() {
    test "$(pending_wals | grep -c .)" -ge "$1"
}

seconds_between() {
    awk -v start="$1" -v end="$2" 'BEGIN { printf "%.3f", end - start }'
}

recovery_promoted() {
    test "$(psql_value 'SELECT NOT pg_is_in_recovery()')" = t
}

record_sample() {
    psql_value "INSERT INTO heartbeat DEFAULT VALUES; INSERT INTO restore_drill_marker(mode) VALUES ('$mode') RETURNING id" | tail -1
}

# Records the first sample and the WAL segment holding it, in the same statement as the insert.
first_sample() {
    psql_value "INSERT INTO heartbeat DEFAULT VALUES; \
        INSERT INTO restore_drill_marker(mode) VALUES ('$mode') \
        RETURNING id || ' ' || pg_walfile_name(pg_current_wal_insert_lsn())" | tail -1
}

marker_epoch() {
    psql_value "SELECT extract(epoch FROM recorded_at)::numeric(20,3) FROM restore_drill_marker WHERE id = $1"
}

seed_reconciliation_fixture() {
    dc exec -T postgres psql -U postgres -d topup -v ON_ERROR_STOP=1 <<'SQL'
CREATE TABLE restore_drill_marker (
    id bigint GENERATED ALWAYS AS IDENTITY PRIMARY KEY,
    mode text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT clock_timestamp()
);

INSERT INTO products (id, slug, webhook_url, pubkey)
VALUES (
    '11111111-1111-1111-1111-111111111111',
    'restore-drill',
    'http://mock-product:8081/webhooks',
    'restore-drill-key'
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

startup_base_backup_listed() {
    dc exec -T backup sh -c 'wal-g backup-list --json | jq -e "length > 0"'
}

# Full object listing with modification times, to prove a drill instance wrote nothing.
storage_listing() {
    dc run --rm --no-deps restore 'wal-g st ls -r' | sort
}

# Creates the overlay's project volumes and copies the drill inputs into them through the API.
seed_drill_volumes() {
    local volume
    for volume in drill_routes drill_mock_product; do
        docker volume create \
            --label "com.docker.compose.project=$project" \
            --label "com.docker.compose.volume=$volume" \
            "${project}_$volume" >/dev/null
    done
    docker create --name "$seed_container" \
        --label "com.docker.compose.project=$project" \
        --volume "${project}_drill_routes:/seed/routes" \
        --volume "${project}_drill_mock_product:/seed/mock-product" \
        --entrypoint /bin/true "$TOPUP_LOCAL_POSTGRES_IMAGE" >/dev/null
    docker cp "$routes_dir/phala-cloud-sepolia-pha.yaml" "$seed_container:/seed/routes/"
    docker cp "$root/deploy/local/mock-product.py" "$seed_container:/seed/mock-product/"
    docker rm "$seed_container" >/dev/null
}

remove_volume() {
    volume=$(docker volume ls -q \
        --filter "label=com.docker.compose.project=$project" \
        --filter "label=com.docker.compose.volume=$1")
    if [ -z "$volume" ]; then
        echo "could not locate drill volume $1" >&2
        return 1
    fi
    docker volume rm "$volume" >/dev/null
}

remove_pgdata_volume() {
    remove_volume pgdata
}

# The heartbeat writer, booted in the restore-check variant (TOPUP_SERVICE_ENABLED=read-only),
# must exit at its configuration check.
failed_closed() {
    dc logs --no-log-prefix "$1" 2>/dev/null |
        grep -F "$2 is disabled while TOPUP_SERVICE_ENABLED=read-only"
}

backup_idle() {
    dc logs --no-log-prefix backup 2>/dev/null |
        grep -Fx 'base backups are disabled while TOPUP_RESTORE_FROM_BACKUP=on'
}

# The drill publishes no ports; the mock product's Python reaches topup on the compose network.
topup_request() {
    dc exec -T mock-product python3 - "$1" "$2" <<'PY'
import sys, urllib.error, urllib.request
request = urllib.request.Request("http://topup:8080" + sys.argv[2], method=sys.argv[1])
try:
    with urllib.request.urlopen(request, timeout=10) as response:
        print(response.status)
        print(response.read().decode())
except urllib.error.HTTPError as error:
    print(error.code)
PY
}

topup_status() {
    topup_request "$1" "$2" | head -1
}

topup_get() {
    topup_request GET "$1" | tail -n +2
}

restore_report_served() {
    topup_get /healthz | jq -e '.restore_check != null'
}

# The restored cluster's application login, with the password the replacement derived.
app_login_works() {
    dc exec -T postgres sh -c '
        PGPASSFILE=/run/db-app/topup_service.pgpass \
            psql -h postgres -U topup_service -d topup -XAtq -c "SELECT current_user"
    '
}

storage_write_probe() {
    dc run --rm --no-deps restore \
        'printf probe >/tmp/probe && wal-g st put --no-compress --no-encrypt /tmp/probe drill-write-probe'
}

# Positive control with the source credentials, so the read-only check below cannot pass on a
# broken probe command.
storage_probe_writes() {
    storage_write_probe >/dev/null 2>&1 || {
        echo "object-storage write probe failed with read-write credentials" >&2
        return 1
    }
    dc run --rm --no-deps restore 'wal-g st rm drill-write-probe' >/dev/null
}

# The replacement's storage credentials must not be able to write.
storage_is_read_only() {
    if storage_write_probe >/dev/null 2>&1; then
        echo "replacement object-storage credentials can write" >&2
        return 1
    fi
}

test_restore_failures_are_fatal() {
    wal_name=$1
    expected_version=$2
    dc exec -T -e TOPUP_BACKUP_KEY_VERSION=0 backup walg-key-manifest wal "$wal_name" >/dev/null
    set +e
    dc exec -T backup walg-restore-command "$wal_name" /tmp/wrong-key-wal >/dev/null 2>&1
    status=$?
    set -e
    test "$status" -eq 126 || {
        echo "wrong WAL key returned $status instead of 126" >&2
        return 1
    }
    dc exec -T -e TOPUP_BACKUP_KEY_VERSION="$expected_version" \
        backup walg-key-manifest wal "$wal_name" >/dev/null
}

upload_pending_wal() {
    version=$1
    wal_name=$2
    dc run --rm --no-deps \
        -e TOPUP_BACKUP_KEY_VERSION="$version" \
        -e WAL_NAME="$wal_name" restore '
        walg-wal-push "$PGDATA/pg_wal/$WAL_NAME"
        mv "$PGDATA/pg_wal/archive_status/$WAL_NAME.ready" \
            "$PGDATA/pg_wal/archive_status/$WAL_NAME.done"
    ' >/dev/null
}

manifest_version() {
    dc exec -T backup wal-g st cat "key-versions/wal/$1.json" | jq -er '.key_version'
}

# Decrypts every rotation segment with its own key version and proves the other version fails.
verify_rotation_keys() {
    dc run --rm --no-deps -e V1_WALS="$1" -e V2_WALS="$2" restore '
        fetch() {
            rm -f /tmp/wal
            WALG_LIBSODIUM_KEY_PATH=/run/wal-g/backup-v$2.key WALG_DOWNLOAD_CONCURRENCY=1 \
                wal-g wal-fetch "$1" /tmp/wal >/dev/null 2>&1 && test -s /tmp/wal
        }
        check() {
            fetch "$1" "$2" || { echo "WAL $1 does not decrypt with backup/v$2" >&2; exit 1; }
            if fetch "$1" "$3"; then
                echo "WAL $1 also decrypts with backup/v$3" >&2
                exit 1
            fi
        }
        for wal in $V1_WALS; do check "$wal" 1 2; done
        for wal in $V2_WALS; do check "$wal" 2 1; done
    '
}

# Builds a WAL backlog under key v1 with object storage down, archives exactly one segment with
# one v1 wrapper call, rotates PostgreSQL to v2, and lets its archiver finish the backlog.
exercise_key_rotation() {
    dc stop s3 >/dev/null
    for _ in 1 2 3 4; do
        record_sample >/dev/null
        psql_value 'SELECT pg_switch_wal()' >/dev/null
    done
    wait_for "WAL backlog" pending_wal_count_at_least 3
    rotation_wals=$(pending_wals)
    dc stop postgres >/dev/null
    dc start s3 >/dev/null
    wait_for s3 dc exec -T s3 /garage bucket info topup-backups

    set -- $rotation_wals
    test "$#" -ge 3 || {
        echo "key rotation test needs at least three pending WAL segments" >&2
        return 1
    }
    rotation_v1_wal=$1
    shift
    rotation_v2_wals=$*
    upload_pending_wal 1 "$rotation_v1_wal"
    test "$(manifest_version "$rotation_v1_wal")" -eq 1
    # Positive control: the listing used below must see the segment that was just archived.
    wal_object_visible "$rotation_v1_wal" >/dev/null || {
        echo "WAL object listing did not show archived segment $rotation_v1_wal" >&2
        return 1
    }
    for wal_name in $rotation_v2_wals; do
        if wal_manifest_visible "$wal_name" >/dev/null 2>&1 || \
            wal_object_visible "$wal_name" >/dev/null 2>&1; then
            echo "WAL-G uploaded pending segment $wal_name outside its own archive call" >&2
            return 1
        fi
    done

    export TOPUP_BACKUP_KEY_VERSION=2
    dc rm -f postgres >/dev/null
    dc up -d --no-deps postgres
    wait_for postgres dc exec -T postgres pg_isready -U postgres -d topup
    for wal_name in $rotation_v2_wals; do
        wait_for "v2 archive of $wal_name" wal_manifest_visible "$wal_name"
        test "$(manifest_version "$wal_name")" -eq 2
    done
    verify_rotation_keys "$rotation_v1_wal" "$rotation_v2_wals"
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
export TOPUP_BACKUP_KEY_VERSION=2
export TOPUP_BACKUP_KEY_FALLBACK_VERSIONS=1,0

routes_dir=$(mktemp -d)
# The seeded deposit belongs to the `restore-drill` product; the attested route is the only source
# of its settlement endpoint, so the drill route names that product and the mock product.
sed -e 's/0x0000000000000000000000000000000000000000/0x3333333333333333333333333333333333333333/g' \
    -e 's|^  product: .*|  product: restore-drill|' \
    -e 's|^  settlement_url: .*|  settlement_url: "http://mock-product:8081/settlements"|' \
    -e 's|^  product_kid: .*|  product_kid: product/restore-drill|' \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" \
    >"$routes_dir/phala-cloud-sepolia-pha.yaml"
chmod 0644 "$routes_dir/phala-cloud-sepolia-pha.yaml"

compose_version=$(docker compose version --short)
if [ "$(printf '%s\n' 2.24.4 "$compose_version" | sort -V | head -1)" != 2.24.4 ]; then
    echo "Docker Compose $compose_version is too old; the drill overlay needs 2.24.4+ (!override)" >&2
    exit 1
fi
dc --profile tools config --format json |
    jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' >/dev/null || {
    echo "the drill stack bind-mounts a host path; CI's Docker daemon cannot see it" >&2
    exit 1
}

dc build postgres dstack-simulator topup
seed_drill_volumes
dc up -d keys s3-init mock-product
wait_for keys dc exec -T keys topup keys --check \
    --backup-dir /run/wal-g --owner-dir /run/db-owner --app-dir /run/db-app
export TOPUP_BACKUP_KEY_VERSION=1
dc up -d --no-deps postgres
wait_for postgres dc exec -T postgres pg_isready -U postgres -d topup
# The object store is empty, so the bootstrap listed no base backup and initialized a new cluster.
dc logs --no-log-prefix postgres 2>&1 |
    grep -Fx 'the backup prefix holds no base backup; initializing a new cluster' >/dev/null || {
    echo "the source did not initialize from a provably empty backup prefix" >&2
    exit 1
}
dc up -d --no-deps backup
# A new cluster has no base backup on its timeline, so backup takes one at start.
wait_for "startup base backup" startup_base_backup_listed
wait_for mock-product dc exec -T mock-product python3 -c \
    "import urllib.request; urllib.request.urlopen('http://localhost:8081/health')"
dc run --rm --no-deps migrate >/dev/null
seed_reconciliation_fixture

backup_name=$(dc exec -T backup walg-base-backup /var/lib/postgresql/data | tail -1)
case "$backup_name" in
    base_*) ;;
    *) echo "could not determine WAL-G base backup name" >&2; exit 1 ;;
esac

if [ "$mode" = controlled ]; then
    exercise_key_rotation
else
    rotation_v1_wal=not-run
    rotation_v2_wals=not-run
fi

# Time the segment that holds the first write, not whichever segment is current beforehand.
first=$(first_sample)
read -r first_marker timed_wal <<<"$first"
test -n "$first_marker" && test -n "$timed_wal"
if [ "$mode" = controlled ]; then
    last_marker=$(record_sample)
    psql_value 'SELECT pg_switch_wal()' >/dev/null
    wait_for_fast "forced WAL close" wal_closed "$timed_wal"
else
    samples_file=$(mktemp)
    printf '%s\n' "$first_marker" >"$samples_file"
    (
        while :; do
            sleep 1
            record_sample >>"$samples_file"
        done
    ) &
    writer_pid=$!
    wait_for_fast "archive_timeout WAL close" wal_closed "$timed_wal"
fi
wait_for_fast "archived WAL metadata" wal_manifest_visible "$timed_wal"
if [ "$mode" = crash ]; then
    kill "$writer_pid" >/dev/null 2>&1 || true
    wait "$writer_pid" >/dev/null 2>&1 || true
    writer_pid=
    last_marker=$(tail -1 "$samples_file")
    rm -f "$samples_file"
    samples_file=
fi
# Server-side timestamps: first write into the segment, segment close, and object upload.
first_write_at=$(marker_epoch "$first_marker")
test -n "$segment_closed_at"
object_uploaded_at=$(wal_object_uploaded_epoch "$timed_wal")
archive_wait_seconds=$(seconds_between "$first_write_at" "$segment_closed_at")
upload_latency_seconds=$(seconds_between "$segment_closed_at" "$object_uploaded_at")
# File mtimes come from the kernel's coarse clock, up to one tick (<=10 ms) behind clock_timestamp().
# Each interval spans at least one psql round trip, so anything below that tolerance is an error.
for seconds in "$archive_wait_seconds" "$upload_latency_seconds"; do
    awk -v value="$seconds" 'BEGIN { exit !(value >= -0.010) }' || {
        echo "WAL timing is negative (${seconds}s); the timed segment is wrong" >&2
        exit 1
    }
done

expected_heartbeat_at=$(psql_value \
    "SELECT to_char(max(recorded_at) AT TIME ZONE 'UTC', 'YYYY-MM-DD\"T\"HH24:MI:SS.US\"Z\"') FROM heartbeat")
expected_lsn=$(psql_value 'SELECT pg_current_wal_lsn()')
expected_marker=$(psql_value 'SELECT max(id) FROM restore_drill_marker')
last_archived_wal=$(psql_value 'SELECT last_archived_wal FROM pg_stat_archiver')
test -n "$last_archived_wal"
test_restore_failures_are_fatal "$last_archived_wal" "$TOPUP_BACKUP_KEY_VERSION"

storage_probe_writes

rto_started=$(date +%s)
dc stop backup >/dev/null
if [ "$mode" = crash ]; then
    docker kill "${project}-postgres-1" >/dev/null
else
    dc stop postgres >/dev/null
fi
dc rm -f backup postgres heartbeat migrate restore-check >/dev/null 2>&1 || true
remove_pgdata_volume
# The key tmpfs volumes die with the source CVM; the replacement derives the backup keys and
# database credentials again, which only the same app id (here: the same simulator keys) reproduces.
dc rm -s -f keys >/dev/null
for volume in walg_key db_owner db_app; do
    remove_volume "$volume"
done

# Replacement boot, exactly as dstack's app-compose.sh starts a CVM: the whole restore-check
# variant of deploy/RESTORE.md comes up at once with read-only object-storage credentials, its only
# sealed difference. PostgreSQL restores the newest base backup into the empty volume, replays
# every archived segment, and never archives; `backup` idles; topup is read-only. No command runs
# inside the stack. Nothing may reach object storage from here on.
storage_before=$(storage_listing)
test -n "$storage_before"
variant=(--restore-check)
export TOPUP_LOCAL_S3_ACCESS_KEY_ID=topup-restore-read
export TOPUP_LOCAL_S3_SECRET_ACCESS_KEY=topup-restore-read-secret
dc up --remove-orphans -d
dc logs --no-log-prefix postgres 2>&1 |
    grep -Fx "restoring base backup $backup_name" >/dev/null || {
    echo "the replacement did not restore the newest base backup $backup_name" >&2
    exit 1
}
recovery_promoted
test "$(psql_value 'SHOW archive_mode')" = off
wait_for "heartbeat failing closed" failed_closed heartbeat heartbeat
wait_for "backup idling" backup_idle
storage_is_read_only

# The operator's only view of the replacement: /healthz and the read API on its gateway URL.
wait_for "restore-check report on /healthz" restore_report_served
health=$(topup_get /healthz)
restore_report=$(printf '%s\n' "$health" | jq -ce '.restore_check')
printf '%s\n' "$health" | jq -e '.mode == "read-only"' >/dev/null
test "$(printf '%s\n' "$restore_report" | jq -er '.status')" = ok || {
    printf 'restore-check: %s\n' "$restore_report" >&2
    exit 1
}
test "$(topup_status POST /v1/admin/products)" = 503
test "$(topup_status GET /v1/products/restore-drill/deposits?tx_hash=0x00)" = 401

# restore-check logged in as the owner, and the application login works too: the restored roles
# carry the source's derived passwords, which the replacement derived again.
test "$(app_login_works)" = topup_service

# The boot-time report is unanchored; compare it with the source point recorded above, as the
# operator compares it with theirs.
restored_heartbeat_at=$(printf '%s\n' "$restore_report" | jq -er '.restored_heartbeat_at')
measured_rpo=$(( $(date -u -d "$expected_heartbeat_at" +%s) - $(date -u -d "$restored_heartbeat_at" +%s) ))
if [ "$measured_rpo" -lt 0 ]; then
    measured_rpo=0
fi
allowed_rpo=$(printf '%s\n' "$restore_report" | jq -er '.allowed_rpo_seconds')
latest_applied_lsn=$(printf '%s\n' "$restore_report" | jq -er '.latest_applied_lsn')
wal_bytes_behind=$(psql_value \
    "SELECT GREATEST(pg_wal_lsn_diff('$expected_lsn', '$latest_applied_lsn'), 0)::bigint")
test "$(printf '%s\n' "$restore_report" | jq -er '.rpo_basis')" = unanchored
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

# Promotion wrote a new timeline; close its segment and confirm nothing reached object storage.
psql_value 'SELECT pg_switch_wal()' >/dev/null
psql_value 'CHECKPOINT' >/dev/null
restored_timeline=$(psql_value 'SELECT timeline_id FROM pg_control_checkpoint()')
test "$restored_timeline" -gt 1
test "$(storage_listing)" = "$storage_before" || {
    echo "the restore-check instance changed object storage" >&2
    exit 1
}

printf 'mode=%s\n' "$mode"
printf 'restore-check: %s\n' "$restore_report"
printf 'base_backup=%s\n' "$backup_name"
printf 'source_marker_range=%s..%s expected_last=%s restored_last=%s\n' \
    "$first_marker" "$last_marker" "$expected_marker" "$restored_marker"
printf 'measured_rpo_seconds=%s\n' "$measured_rpo"
printf 'allowed_rpo_with_sampling_seconds=%s\n' "$allowed_rpo"
printf 'wal_bytes_behind=%s\n' "$wal_bytes_behind"
printf 'archive_window_seconds=60\n'
printf 'archive_wait_seconds=%s\n' "$archive_wait_seconds"
printf 'upload_latency_seconds=%s\n' "$upload_latency_seconds"
printf 'key_rotation_v1_wal=%s\n' "$rotation_v1_wal"
printf 'key_rotation_v2_wals=%s\n' "$rotation_v2_wals"
printf 'restored_timeline=%s storage_unchanged_by_restore_check=true\n' "$restored_timeline"
printf 'elapsed_rto_seconds=%s\n' "$rto_elapsed"
echo "restore drill $mode passed"
