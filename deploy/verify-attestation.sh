#!/bin/sh
set -eu

# Verifies a deployed CVM with the official dstack verifier (dstack-verifier.sh): the TDX quote
# and TCB, the RTMR3 event-log replay, and the OS image measurements. The replayed app id must be
# APP_ID and the replayed compose hash the SHA-256 of the attested app-compose, whose
# docker_compose_file must be EXPECTED_COMPOSE byte for byte. Then the compose policy: without
# ENV_EXAMPLE, the topup compose (allowed_envs from app-compose.example.json, credential isolation,
# the single 8080 ingress); with ENV_EXAMPLE and SERVICE:PORT (the reference product),
# allowed_envs exactly ENV_EXAMPLE's names and SERVICE:PORT the only published port.
#
# ATTESTATION_JSON is `phala cvms attestation --json` (the app certificate's quote, the event log,
# and the app-compose). INFO_JSON is the guest agent's public `GET /prpc/Info` on port 8090 (the
# CVM runs with public tcbinfo): the vm_config the quote does not carry.
if [ "$#" -ne 4 ] && [ "$#" -ne 6 ]; then
    echo "usage: verify-attestation.sh ATTESTATION_JSON INFO_JSON APP_ID EXPECTED_COMPOSE [ENV_EXAMPLE SERVICE:PORT]" >&2
    exit 64
fi

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
attestation=$1
info=$2
app_id=$(printf '%s' "${3#0x}" | tr 'A-F' 'a-f')
expected_compose=$4
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

jq -e --slurpfile info "$info" '{
    attestation: null,
    quote: [.app_certificates[] | select(.position_in_chain == 0) | .quote][0],
    event_log: (.tcb_info.event_log | tojson),
    vm_config: $info[0].vm_config
} | select((.quote | type) == "string" and (.vm_config | type) == "string" and .vm_config != "")' \
    "$attestation" >"$tmp/request.json" || {
    echo "the attestation has no app certificate quote, or the guest agent info no vm_config" >&2
    exit 1
}
"$root/deploy/dstack-verifier.sh" <"$tmp/request.json" >"$tmp/result.json"

jq -jer '.compose_file | select(type == "string" and length > 0)' "$attestation" \
    >"$tmp/app-compose.json"
compose_hash=$(sha256sum "$tmp/app-compose.json" | awk '{print $1}')
jq -e --arg app_id "$app_id" --arg compose_hash "$compose_hash" '
    .details.tcb_status == "UpToDate"
    and .details.app_info.app_id == $app_id
    and .details.app_info.compose_hash == $compose_hash
' "$tmp/result.json" >/dev/null || {
    echo "the verified attestation does not match: expected TCB UpToDate, app id $app_id, compose hash $compose_hash" >&2
    jq '.details | {tcb_status, advisory_ids, app_id: .app_info.app_id, compose_hash: .app_info.compose_hash}' \
        "$tmp/result.json" >&2
    exit 1
}
jq -r '"dstack verifier: quote and TCB \(.details.tcb_status), RTMR3 event log, OS image \(.details.app_info.os_image_hash)",
    "attested app id: \(.details.app_info.app_id)",
    "attested compose hash: 0x\(.details.app_info.compose_hash)"' "$tmp/result.json"

jq -e 'type == "object"' "$tmp/app-compose.json" >/dev/null
jq -jer '.docker_compose_file | select(type == "string" and length > 0)' \
    "$tmp/app-compose.json" >"$tmp/docker-compose.yml"
cmp -s "$expected_compose" "$tmp/docker-compose.yml" || {
    echo "attested docker_compose_file differs from the prepared compose" >&2
    diff -u "$expected_compose" "$tmp/docker-compose.yml" >&2 || true
    exit 1
}

jq -r '.allowed_envs[]' "$tmp/app-compose.json" | sort -u >"$tmp/actual-envs"
if [ "$#" -eq 6 ]; then
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$5" | sort -u >"$tmp/expected-envs"
else
    jq -r '.allowed_envs[]' "$root/deploy/app-compose.example.json" | sort -u >"$tmp/expected-envs"
fi
cmp -s "$tmp/actual-envs" "$tmp/expected-envs" || {
    echo "attested allowed_envs differs from the reviewed allow-list" >&2
    diff -u "$tmp/expected-envs" "$tmp/actual-envs" >&2 || true
    exit 1
}

docker compose -f "$tmp/docker-compose.yml" config --format json >"$tmp/docker-compose.json"
if [ "$#" -eq 6 ]; then
    service=${6%%:*}
    port=${6#*:}
    jq -e --arg service "$service" --argjson port "$port" '
        [.services | to_entries[] | select((.value.ports // []) | length > 0) | .key] == [$service]
        and .services[$service].ports == [{
            "mode": "ingress",
            "target": $port,
            "published": ($port | tostring),
            "protocol": "tcp"
        }]
    ' "$tmp/docker-compose.json" >/dev/null || {
        echo "attested compose does not have the expected single $6 ingress" >&2
        exit 1
    }
    echo "attested compose, allowed_envs, and ingress passed"
    exit 0
fi
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
' "$tmp/docker-compose.json" >/dev/null || {
    echo "attested compose does not have the expected single 8080 ingress policy" >&2
    exit 1
}
echo "attested compose, allowed_envs, credential isolation, and ingress passed"
