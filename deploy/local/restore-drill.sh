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
export TOPUP_LOCAL_SERVICE_IMAGE="phala-pay:$project"
export TOPUP_LOCAL_POSTGRES_IMAGE="phala-pay-postgres-walg:$project"
export TOPUP_LOCAL_DSTACK_IMAGE="phala-pay-dstack-simulator:$project"
writer_pid=
samples_file=
routes_dir=
admin_dir=
switch_lsn=
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
    if [ -n "$admin_dir" ]; then
        rm -rf "$admin_dir"
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

wal_object_visible() {
    dc exec -T backup wal-g st ls wal_005/ | grep -F " $1."
}

# Upload time of a WAL object as recorded by object storage, in epoch seconds with milliseconds.
wal_object_uploaded_epoch() {
    line=$(dc exec -T backup wal-g st ls wal_005/ | grep -F " $1.")
    # shellcheck disable=SC2086  # split the listing line into its fields
    set -- $line
    test "$#" -ge 7 || {
        echo "WAL object listing has no upload time for $1" >&2
        return 1
    }
    date -u -d "$3 $4" +%s.%3N
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

INSERT INTO accounts (id, name)
VALUES ('11111111-1111-1111-1111-111111111111', 'restore-drill');
INSERT INTO customers (id, account_id, livemode, client_reference_id)
VALUES (
    '22222222-2222-2222-2222-222222222222',
    '11111111-1111-1111-1111-111111111111',
    true,
    'restore-drill-customer'
);
INSERT INTO quotes (
    id, account_id, livemode, customer_id, route, amount_atomic, price_scaled, credit_minor,
    expires_at, status, closed_at
)
VALUES (
    '55555555-5555-5555-5555-555555555555',
    '11111111-1111-1111-1111-111111111111',
    true,
    '22222222-2222-2222-2222-222222222222',
    'restore-drill',
    1000,
    25000000,
    250,
    '2026-09-22T00:15:00Z',
    'expired',
    '2026-09-22T00:15:00Z'
);
INSERT INTO addresses (
    id, account_id, livemode, chain_id, quote_id, salt, treasury, address
)
VALUES (
    '33333333-3333-3333-3333-333333333333',
    '11111111-1111-1111-1111-111111111111',
    true,
    1,
    '55555555-5555-5555-5555-555555555555',
    '0xeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee',
    '0x0000000000000000000000000000000000007ea5',
    '0xdddddddddddddddddddddddddddddddddddddddd'
);
INSERT INTO deposits (
    id, account_id, livemode, customer_id, chain_id, tx_hash, log_index, receipt_log_index,
    block_number, block_hash, block_time, address_id, route, route_version, asset_contract,
    from_address, amount_atomic, state, next_attempt_at, valuation_at, price_scaled,
    price_source, credit_minor, final_at
)
VALUES (
    '44444444-4444-4444-4444-444444444444',
    '11111111-1111-1111-1111-111111111111',
    true,
    '22222222-2222-2222-2222-222222222222',
    1,
    '0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa',
    0,
    0,
    100,
    '0xffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff',
    '2026-09-22T00:00:00Z',
    '33333333-3333-3333-3333-333333333333',
    'restore-drill',
    1,
    '0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb',
    '0xcccccccccccccccccccccccccccccccccccccccc',
    1000,
    'credited',
    now(),
    '2026-09-22T00:00:00Z',
    25000000,
    'spot',
    250,
    '2026-09-22T00:00:00Z'
);
INSERT INTO heartbeat DEFAULT VALUES;
INSERT INTO restore_drill_marker(mode) VALUES ('base');
SQL
}

# Business consistency after a restore (controlled mode; deploy/runbooks/restore.md): a test-mode
# account whose key revocation, deposit address rotation, and delivered deposit.credited happen
# after the last archived WAL, so the restore loses them. The addresses are the deposit address
# formula's for this account, customer, route factory, and treasury (docs/design/multi-tenant.md
# §5a); the ids are the deterministic deposit and event ids of the transfer (chain 11155111,
# transaction 0x5a…5a, receipt position 0).
consistency_account=66666666-6666-6666-6666-666666666666
consistency_account_id=acct_66666666666666666666666666666666
consistency_treasury=0x0000000000000000000000000000000000007ea6
consistency_salt_v1=0x5364d14f27c908c6861df22196c51b7b693306887fa5984420aeaf04b35527f1
consistency_address_v1=0xcac987989e30d486588c3fcdd059ce71168a8c64
consistency_salt_v2=0xb284965b0e0bc5759251ed751eee59732336798de341530456cbb3c57373f457
consistency_address_v2=0xfbf725ff86da685728ec7275c9fc155b4443ea35
consistency_address_v2_id=da_99999999999999999999999999999992
consistency_event=c371cbc5-44c4-5e44-a795-6242ab4606d9
delivered_event=$(jq -cn --arg address "$consistency_address_v2" '{
    id: "evt_c371cbc544c45e44a7956242ab4606d9", object: "event",
    account: "acct_66666666666666666666666666666666", livemode: false,
    type: "deposit.credited", created: 1790000000, actor: "system",
    data: {object: {
        id: "dep_e2facb389b5c57c69f7501e57d34b8d5", object: "deposit", livemode: false,
        client_reference_id: "restore-drill-da",
        deposit_address: "da_99999999999999999999999999999992", status: "credited",
        chain_id: 11155111, address: $address,
        tx_hash: "0x5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a5a",
        asset_contract: "0x8f40e7e99678f44c88158f049e62817580ab113b",
        from_address: "0x00000000000000000000000000000000000000f7",
        amount_atomic: "1000000000000000000", amount: 25, currency: "usd",
        exchange_rate: "0.25000000", price_source: "spot", valued_at: 1790000000}}}')
kept_key=
lost_key=
public_origin=
delivered_delivery=

# The delivery of the event on stdin as the merchant's receiver records it (Standard Webhooks
# headers and the raw body), signed `v1a` with the account's test-mode webhook key version 1,
# which the simulator derives at the service's dstack path (crates/core/src/signer.rs) and which is
# the ed25519 seed itself.
sign_delivery() {
    local body id timestamp=1790000031
    body=$(jq -c .)
    id=$(jq -r .id <<<"$body")
    dc exec -T mock-product python3 -c '
import base64, http.client, json, socket, sys

class Dstack(http.client.HTTPConnection):
    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.connect("/run/dstack/dstack.sock")

connection = Dstack("dstack")
connection.request("POST", "/GetKey", json.dumps(
    {"path": sys.argv[1], "purpose": "", "algorithm": "secp256k1"}),
    {"Content-Type": "application/json"})
response = connection.getresponse()
if response.status != 200:
    sys.exit("GetKey answered %d" % response.status)
seed = bytes.fromhex(json.loads(response.read())["key"])
if len(seed) != 32:
    sys.exit("the derived key is not 32 bytes")
der = bytes.fromhex("302e020100300506032b657004220420") + seed
print("-----BEGIN PRIVATE KEY-----")
print(base64.b64encode(der).decode())
print("-----END PRIVATE KEY-----")
' "settlement/$consistency_account_id/test/v1" >"$admin_dir/webhook.pem"
    printf '%s.%s.%s' "$id" "$timestamp" "$body" >"$admin_dir/webhook-content"
    jq -cn --arg id "$id" --arg timestamp "$timestamp" --arg body "$body" \
        --arg signature "v1a,$(openssl pkeyutl -sign -rawin -inkey "$admin_dir/webhook.pem" \
            -in "$admin_dir/webhook-content" | openssl base64 -A)" \
        '{webhook_id: $id, webhook_timestamp: $timestamp, webhook_signature: $signature,
          body: $body}'
    rm -f "$admin_dir/webhook.pem" "$admin_dir/webhook-content"
}

# A well-formed test-mode secret key (crates/topup/src/api_keys.rs): 43 random base62 characters
# and the base62 CRC-32 of everything before the checksum.
new_api_key() {
    dc exec -T mock-product python3 -c '
import secrets, zlib
alphabet = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz"
body = "ppay_sk_test_" + "".join(secrets.choice(alphabet) for _ in range(43))
value, checksum = zlib.crc32(body.encode()), ""
for _ in range(6):
    checksum, value = alphabet[value % 62] + checksum, value // 62
print(body + checksum)
'
}

seed_consistency_fixture() {
    kept_key=$(new_api_key)
    lost_key=$(new_api_key)
    dc exec -T postgres psql -U postgres -d topup -v ON_ERROR_STOP=1 \
        -v account="$consistency_account" -v treasury="$consistency_treasury" \
        -v kept="$kept_key" -v lost="$lost_key" \
        -v salt="$consistency_salt_v1" -v address="$consistency_address_v1" <<'SQL'
INSERT INTO accounts (id, name) VALUES (:'account', 'restore-drill-consistency');
INSERT INTO api_keys (id, account_id, livemode, kind, prefix, last4, key_hash, created_by)
VALUES ('77777777-7777-7777-7777-777777777771', :'account', false, 'secret', 'ppay_sk_test_',
        right(:'kept', 4), sha256(convert_to(:'kept', 'UTF8')), 'admin'),
       ('77777777-7777-7777-7777-777777777772', :'account', false, 'secret', 'ppay_sk_test_',
        right(:'lost', 4), sha256(convert_to(:'lost', 'UTF8')), 'admin');
INSERT INTO treasuries (
    id, account_id, livemode, chain_id, address, kind, proof_message, proof_signature,
    verified_at, effective_at, screened_at, applied_at, created_by
)
VALUES ('77777777-7777-7777-7777-777777777773', :'account', false, 11155111, :'treasury', 'eoa',
        'restore drill', '0x', now(), now(), now(), now(), 'key_77777777777777777777777777777771');
INSERT INTO webhook_endpoints (id, account_id, livemode, url)
VALUES ('77777777-7777-7777-7777-777777777774', :'account', false,
        'http://mock-product:8081/webhooks');
INSERT INTO customers (id, account_id, livemode, client_reference_id)
VALUES ('88888888-8888-8888-8888-888888888888', :'account', false, 'restore-drill-da');
INSERT INTO deposit_addresses (id, account_id, livemode, customer_id, version)
VALUES ('99999999-9999-9999-9999-999999999991', :'account', false,
        '88888888-8888-8888-8888-888888888888', 1);
INSERT INTO addresses (
    id, account_id, livemode, chain_id, deposit_address_id, salt, treasury, address
)
VALUES ('99999999-9999-9999-9999-9999999999a1', :'account', false, 11155111,
        '99999999-9999-9999-9999-999999999991', :'salt', :'treasury', :'address');
SQL
}

# After the last archived WAL: the merchant revokes a key, rotates the customer's deposit address,
# and receives deposit.credited, whose delivery its receiver records. None of it reaches object
# storage.
lose_consistency_changes() {
    delivered_delivery=$(sign_delivery <<<"$delivered_event")
    dc exec -T postgres psql -U postgres -d topup -v ON_ERROR_STOP=1 \
        -v account="$consistency_account" -v treasury="$consistency_treasury" \
        -v salt="$consistency_salt_v2" -v address="$consistency_address_v2" \
        -v event="$consistency_event" -v data="$(jq -c .data <<<"$delivered_event")" <<'SQL'
UPDATE api_keys SET revoked_at = now() WHERE id = '77777777-7777-7777-7777-777777777772';
UPDATE deposit_addresses SET status = 'retired', retired_at = now()
WHERE id = '99999999-9999-9999-9999-999999999991';
INSERT INTO deposit_addresses (id, account_id, livemode, customer_id, version)
VALUES ('99999999-9999-9999-9999-999999999992', :'account', false,
        '88888888-8888-8888-8888-888888888888', 2);
INSERT INTO addresses (
    id, account_id, livemode, chain_id, deposit_address_id, salt, treasury, address
)
VALUES ('99999999-9999-9999-9999-9999999999a2', :'account', false, 11155111,
        '99999999-9999-9999-9999-999999999992', :'salt', :'treasury', :'address');
INSERT INTO events (id, account_id, livemode, type, object_type, object_id, actor, data, created)
VALUES (:'event', :'account', false, 'deposit.credited', 'deposit',
        'e2facb38-9b5c-57c6-9f75-01e57d34b8d5', 'system', :'data'::jsonb,
        to_timestamp(1790000000));
SQL
}

# One request to topup on the compose network, its body on stdin; prints the status, the
# Retry-After header or `-`, and the body, one per line.
topup_call() {
    dc exec -T mock-product python3 -c '
import sys, urllib.error, urllib.request
method, path, *headers = sys.argv[1:]
body = sys.stdin.buffer.read()
request = urllib.request.Request("http://topup:8080" + path, data=body or None, method=method)
for header in headers:
    name, value = header.split(": ", 1)
    request.add_header(name, value)
try:
    response = urllib.request.urlopen(request, timeout=10)
except urllib.error.HTTPError as error:
    response = error
print(response.status)
print(response.headers.get("Retry-After") or "-")
print(response.read().decode())
' "$@"
}

# An admin-signed request with the drill's admin key (deploy/runbooks/sign-admin-request.sh), for
# the replacement's TOPUP_PUBLIC_ORIGIN. A signature is single-use, so each is made in its own
# second.
admin_call() {
    sleep 1
    printf '%s' "${3:-}" >"$admin_dir/body"
    local headers
    mapfile -t headers < <("$root/deploy/runbooks/sign-admin-request.sh" "$1" \
        "$public_origin$2" "$admin_dir/body" "$admin_dir/admin.pem" local-admin/v1)
    topup_call "$1" "$2" 'content-type: application/json' "${headers[@]}" <"$admin_dir/body"
}

merchant_call() {
    topup_call "$1" "$2" "authorization: Bearer $3" 'content-type: application/json'
}

call_status() { sed -n 1p <<<"$1"; }
call_retry_after() { sed -n 2p <<<"$1"; }
call_body() { tail -n +3 <<<"$1"; }

expect_call() {
    test "$(call_status "$2")" = "$1" || {
        printf 'expected %s, got: %s\n' "$1" "$2" >&2
        return 1
    }
}

# The replacement is frozen; the operator's reconciliation brings back the lost security change
# and deposit address, and keeps the delivered event as delivered (deploy/runbooks/restore.md).
check_consistency_after_restore() {
    local answer key
    public_origin=$(dc config --format json | jq -er '.services.topup.environment.TOPUP_PUBLIC_ORIGIN')
    answer=$(admin_call GET /v1/admin/restore)
    expect_call 200 "$answer"
    call_body "$answer" | jq -e --arg id "$(jq -r .restore_id <<<"$restore_report")" \
        '.frozen and .restore.detected_by == "restore_check" and .restore.id == $id' >/dev/null
    answer=$(merchant_call POST /v1/deposit_addresses "$kept_key" \
        <<<'{"client_reference_id":"restore-drill-da"}')
    expect_call 503 "$answer"
    test "$(call_retry_after "$answer")" = 300
    call_body "$answer" | jq -e '.error.code == "service_restoring"' >/dev/null

    # The restore made the key revoked after the backup valid again, but while frozen no key
    # authenticates, reads included; it is revoked again, by prefix, before the unfreeze.
    local lost_revoked="SELECT revoked_at IS NOT NULL FROM api_keys \
        WHERE id = '77777777-7777-7777-7777-777777777772'"
    test "$(psql_value "$lost_revoked")" = f || {
        echo "the key revocation was not lost: the segment holding it was archived" >&2
        return 1
    }
    for key in "$lost_key" "$kept_key"; do
        answer=$(merchant_call GET /v1/account "$key" </dev/null)
        expect_call 503 "$answer"
        call_body "$answer" | jq -e '.error.code == "service_restoring"' >/dev/null
    done
    answer=$(admin_call POST /v1/admin/restore/api_keys/revoke "$(jq -cn \
        --arg account "$consistency_account_id" --arg last4 "${lost_key: -4}" \
        '{account: $account, prefix: "ppay_sk_test_", last4: $last4,
          reason: "restore drill: revoked after the backup"}')")
    expect_call 200 "$answer"
    call_body "$answer" | jq -e '.status == "revoked"' >/dev/null
    test "$(psql_value "$lost_revoked")" = t

    # The address given out after the backup is re-issued identically from the merchant's record.
    test "$(psql_value "SELECT count(*) FROM deposit_addresses WHERE version = 2")" = 0
    answer=$(admin_call POST /v1/admin/restore/deposit_addresses "$(jq -cn \
        --arg account "$consistency_account_id" --arg address "$consistency_address_v2" \
        --arg id "$consistency_address_v2_id" \
        '{account: $account, livemode: false, client_reference_id: "restore-drill-da",
          address: $address, id: $id, reason: "restore drill: issued after the backup"}')")
    expect_call 200 "$answer"
    call_body "$answer" | jq -e --arg address "$consistency_address_v2" \
        --arg id "$consistency_address_v2_id" \
        '.reissued and .deposit_address.id == $id and .deposit_address.version == 2
         and .deposit_address.address == $address and .deposit_address.status == "active"' \
        >/dev/null

    # The event delivered after the backup is imported from its signed delivery, as delivered, and
    # never sent again; a body changed after signing is refused and changes nothing.
    test "$(psql_value "SELECT count(*) FROM events WHERE id = '$consistency_event'")" = 0
    answer=$(admin_call POST /v1/admin/restore/events \
        "$(jq -c '{deliveries: [.], reason: "restore drill: delivered after the backup"}' \
            <<<"$delivered_delivery")")
    expect_call 200 "$answer"
    call_body "$answer" | jq -e '.data == [{id: "evt_c371cbc544c45e44a7956242ab4606d9",
        result: "imported"}]' >/dev/null
    answer=$(admin_call POST /v1/admin/restore/events \
        "$(jq -c '.body |= (fromjson | .data.object.amount = 26 | tojson)
            | {deliveries: [.], reason: "restore drill: re-valued"}' <<<"$delivered_delivery")")
    expect_call 400 "$answer"
    call_body "$answer" | jq -e '.error.param == "deliveries"' >/dev/null
    test "$(psql_value "SELECT (data = '$(jq -c .data <<<"$delivered_event")'::jsonb)::text \
        || ':' || (SELECT count(*) FROM webhook_deliveries WHERE event_id = '$consistency_event') \
        FROM events WHERE id = '$consistency_event'")" = 'true:0'

    # Nothing scans on the restore-check instance, so the freeze cannot be lifted there.
    answer=$(admin_call POST /v1/admin/restore/unfreeze '{"reason":"restore drill",
        "security_changes_reapplied":true,"deposit_addresses_reissued":true,
        "delivered_events_imported":true}')
    expect_call 400 "$answer"
    call_body "$answer" | jq -e '.error.code == "restore_rescan_incomplete"' >/dev/null
    test "$(psql_value 'SELECT count(*) FROM restores WHERE unfrozen_at IS NULL')" = 1
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
    local volume route
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
    for route in "$routes_dir"/*.yaml; do
        docker cp "$route" "$seed_container:/seed/routes/"
    done
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

# restore_command must abort recovery (126), not end it, when a segment does not decrypt: a wrong
# key must never promote a partial restore. The same call with the backup key is the control.
test_restore_failures_are_fatal() {
    set +e
    dc exec -T backup sh -c '
        od -An -tx1 -N32 /dev/urandom | tr -d " \n" >/tmp/wrong.key
        WALG_LIBSODIUM_KEY_PATH=/tmp/wrong.key walg-restore-command "$1" /tmp/wrong-key-wal
    ' sh "$1" >/dev/null 2>&1
    status=$?
    set -e
    test "$status" -eq 126 || {
        echo "wrong WAL key returned $status instead of 126" >&2
        return 1
    }
    dc exec -T backup walg-restore-command "$1" /tmp/restored-wal >/dev/null 2>&1 || {
        echo "WAL $1 does not restore with the backup key" >&2
        return 1
    }
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}

routes_dir=$(mktemp -d)
# The replacement's admin key, for the operator's restore reconciliation.
admin_dir=$(mktemp -d)
openssl genpkey -algorithm ed25519 -out "$admin_dir/admin.pem" 2>/dev/null
TOPUP_LOCAL_ADMIN_PUBLIC_KEY=$(openssl pkey -in "$admin_dir/admin.pem" -pubout -outform DER |
    tail -c 32 | base64)
export TOPUP_LOCAL_ADMIN_PUBLIC_KEY TOPUP_LOCAL_ADMIN_KID=local-admin/v1
for route in "$root"/deploy/config/routes/*.yaml; do
    sed -e 's/0x0000000000000000000000000000000000000000/0x3333333333333333333333333333333333333333/g' \
        "$route" >"$routes_dir/$(basename "$route")"
    chmod 0644 "$routes_dir/$(basename "$route")"
done

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
if [ "$mode" = controlled ]; then
    seed_consistency_fixture
fi

# WAL-G 3.0.9 `backup-list --json` has `backup_name` and `time`; the newest is the one just pushed.
backup_name=$(dc exec -T backup sh -c 'wal-g backup-push "$PGDATA" >&2 && wal-g backup-list --json' |
    jq -er 'max_by(.time | sub("[.][0-9]+"; "") | fromdateiso8601) | .backup_name')
case "$backup_name" in
    base_*) ;;
    *) echo "could not determine WAL-G base backup name" >&2; exit 1 ;;
esac

# Time the segment that holds the first write, not whichever segment is current beforehand.
first=$(first_sample)
read -r first_marker timed_wal <<<"$first"
test -n "$first_marker" && test -n "$timed_wal"
if [ "$mode" = controlled ]; then
    last_marker=$(record_sample)
    # The end of the switched segment: everything up to it is archived, so it is restored.
    switch_lsn=$(psql_value 'SELECT pg_switch_wal()')
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
wait_for_fast "archived WAL" wal_object_visible "$timed_wal"
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
if [ "$mode" = controlled ]; then
    expected_lsn=$switch_lsn
else
    expected_lsn=$(psql_value 'SELECT pg_current_wal_lsn()')
fi
expected_marker=$(psql_value 'SELECT max(id) FROM restore_drill_marker')
last_archived_wal=$(psql_value 'SELECT last_archived_wal FROM pg_stat_archiver')
test -n "$last_archived_wal"
test_restore_failures_are_fatal "$last_archived_wal"

storage_probe_writes

if [ "$mode" = controlled ]; then
    lose_consistency_changes
fi
rto_started=$(date +%s)
dc stop backup >/dev/null
# The controlled source is killed too, once its consistency writes are made: a clean shutdown
# switches and archives the last segment (PostgreSQL's ShutdownXLOG with archiving on), which would
# keep them.
docker kill "${project}-postgres-1" >/dev/null
dc rm -f backup postgres heartbeat migrate restore-check >/dev/null 2>&1 || true
remove_pgdata_volume
# The key tmpfs volumes die with the source CVM; the replacement derives the backup key and
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

# The operator's only view of the replacement: /healthz and the read API on its restore URL (in a
# CVM, port 8081 of the app's gateway URL, which the attested variant publishes instead of running
# dstack-ingress; validate-compose.sh checks that; here, topup's container port on the compose
# network).
wait_for "restore-check report on /healthz" restore_report_served
health=$(topup_get /healthz)
restore_report=$(printf '%s\n' "$health" | jq -ce '.restore_check')
printf '%s\n' "$health" | jq -e '.mode == "read-only"' >/dev/null
test "$(printf '%s\n' "$restore_report" | jq -er '.status')" = ok || {
    printf 'restore-check: %s\n' "$restore_report" >&2
    exit 1
}
test "$(topup_status POST /v1/admin/accounts)" = 503
# Frozen by restore-check: no merchant request is authenticated, reads included.
test "$(topup_status GET '/v1/deposits?tx_hash=0x00')" = 503

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
# The service's own record is authoritative: the restore asks the product nothing and keeps it.
test "$restored_pricing" = 'credited|250|25000000'
test "$measured_rpo" -le "$allowed_rpo"
test "$rto_elapsed" -le 3600
if [ "$mode" = controlled ]; then
    test "$restored_marker" -eq "$expected_marker"
    test "$wal_bytes_behind" -eq 0
    check_consistency_after_restore
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
printf 'restored_timeline=%s storage_unchanged_by_restore_check=true\n' "$restored_timeline"
printf 'elapsed_rto_seconds=%s\n' "$rto_elapsed"
if [ "$mode" = controlled ]; then
    echo 'restore_mode=frozen, merchant reads refused; lost key revoked again; lost deposit address re-issued identically; signed delivered event kept as delivered'
fi
echo "restore drill $mode passed"
