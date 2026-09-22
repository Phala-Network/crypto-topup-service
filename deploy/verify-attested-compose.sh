#!/bin/sh
set -eu

if [ "$#" -ne 3 ]; then
    echo "usage: verify-attested-compose.sh ATTESTATION_JSON CVM_JSON EXPECTED_COMPOSE" >&2
    exit 64
fi

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
attestation=$1
cvm=$2
expected_compose=$3
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

app_compose="$tmp/app-compose.json"
deployed_compose="$tmp/docker-compose.yml"
compose_config="$tmp/docker-compose.json"
actual_envs="$tmp/actual-envs"
expected_envs="$tmp/expected-envs"

jq -jer '.compose_file | select(type == "string" and length > 0)' \
    "$attestation" >"$app_compose"
jq -e 'type == "object"' "$app_compose" >/dev/null
jq -jer '.docker_compose_file | select(type == "string" and length > 0)' \
    "$app_compose" >"$deployed_compose"

cmp -s "$expected_compose" "$deployed_compose" || {
    echo "attested docker_compose_file differs from the prepared compose" >&2
    diff -u "$expected_compose" "$deployed_compose" >&2 || true
    exit 1
}

actual_hash="0x$("$root/deploy/compose-hash.sh" "$app_compose")"
reported_hash=$(jq -er '.compose_hash // .data.compose_hash' "$cvm")
case "$reported_hash" in
    0x*) ;;
    *) reported_hash="0x$reported_hash" ;;
esac
if [ "$actual_hash" != "$reported_hash" ]; then
    echo "attested app-compose hash differs from the CVM compose hash" >&2
    echo "attested: $actual_hash" >&2
    echo "reported: $reported_hash" >&2
    exit 1
fi

jq -r '.allowed_envs[]' "$app_compose" | sort -u >"$actual_envs"
jq -r '.allowed_envs[]' "$root/deploy/app-compose.example.json" | sort -u \
    >"$expected_envs"
cmp -s "$actual_envs" "$expected_envs" || {
    echo "attested allowed_envs differs from the reviewed allow-list" >&2
    diff -u "$expected_envs" "$actual_envs" >&2 || true
    exit 1
}

docker compose -f "$deployed_compose" config --format json >"$compose_config"
jq -e '
    (.services.topup.environment | has("MIGRATE_DATABASE_URL") | not) and
    ((.services.postgres.ports // []) | length == 0) and
    ((.services.migrate.ports // []) | length == 0) and
    ((.services.backup.ports // []) | length == 0) and
    ((.services.topup.ports // []) == [{
        "mode": "ingress",
        "target": 8080,
        "published": "8080",
        "protocol": "tcp"
    }])
' "$compose_config" >/dev/null || {
    echo "attested compose does not have the expected single 8080 ingress policy" >&2
    exit 1
}

echo "attested compose hash: $actual_hash"
echo "attested compose, allowed_envs, credential isolation, and ingress passed"
