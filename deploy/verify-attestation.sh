#!/bin/sh
set -eu

# Verifies a deployed CVM with the official dstack verifier (dstack-verifier.sh): the TDX quote
# and TCB, the RTMR3 event-log replay, and the OS image measurements. The replayed app id must be
# APP_ID and the replayed compose hash the SHA-256 of the attested app-compose (the full
# app-compose JSON the Phala CLI built, not the compose file), whose docker_compose_file must be
# EXPECTED_COMPOSE byte for byte. Then the compose's own policy for VARIANT (`service`,
# `restore-check`, or `template` for topup, `product` for the reference product):
# deploy/compose-policy.jq, and allowed_envs exactly the compose's `${NAME:-}` names. The app-compose's
# pre_launch_script, which the guest sources before `docker compose up` and which could change what
# runs, must be absent or one of the reviewed scripts of deploy/pre-launch-scripts.json, by SHA-256.
# The template's DSTACK_APP_DOMAIN is not an allowed env: the reviewed pre-launch script exports it
# from the app id and the gateway domain.
#
# ATTESTATION_JSON is `phala cvms attestation --json` (the app certificate's quote, the event log,
# and the app-compose). INFO_JSON is the guest agent's public `GET /prpc/Info` on port 8090 (the
# CVM runs with public tcbinfo): the vm_config the quote does not carry.
if [ "$#" -ne 5 ]; then
    echo "usage: verify-attestation.sh ATTESTATION_JSON INFO_JSON APP_ID EXPECTED_COMPOSE service|restore-check|template|product" >&2
    exit 64
fi
case "$5" in
    service | restore-check | template | product) variant=$5 ;;
    *) echo "unknown variant $5" >&2; exit 64 ;;
esac

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
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
if jq -e 'has("pre_launch_script")' "$tmp/app-compose.json" >/dev/null; then
    jq -j '.pre_launch_script' "$tmp/app-compose.json" >"$tmp/pre-launch.sh"
    pre_launch=$(sha256sum "$tmp/pre-launch.sh" | awk '{print $1}')
    name=$(jq -r --arg sha256 "$pre_launch" '.scripts[] | select(.sha256 == $sha256) | .name' \
        "$root/deploy/pre-launch-scripts.json")
    [ -n "$name" ] || {
        echo "the attested pre_launch_script (sha256 $pre_launch) is not a reviewed script of deploy/pre-launch-scripts.json" >&2
        exit 1
    }
    echo "attested pre-launch script: $name"
else
    echo "attested pre-launch script: none"
fi
jq -jer '.docker_compose_file | select(type == "string" and length > 0)' \
    "$tmp/app-compose.json" >"$tmp/docker-compose.yml"
cmp -s "$expected_compose" "$tmp/docker-compose.yml" || {
    echo "attested docker_compose_file differs from the prepared compose" >&2
    diff -u "$expected_compose" "$tmp/docker-compose.yml" >&2 || true
    exit 1
}

compose=$("$root/deploy/pinned-compose.sh")
"$compose" -f "$tmp/docker-compose.yml" config --no-interpolate --format json >"$tmp/docker-compose.json"
# Every setting is attested: the env holds only the compose's sealed names, and exactly those.
"$compose" -f "$tmp/docker-compose.yml" config --variables |
    awk -v variant="$variant" 'NR > 1 && NF > 0 && !(variant == "template" && $1 == "DSTACK_APP_DOMAIN") { print $1 }' |
    sort -u >"$tmp/expected-envs"
jq -r '.allowed_envs[]' "$tmp/app-compose.json" | sort -u >"$tmp/actual-envs"
cmp -s "$tmp/actual-envs" "$tmp/expected-envs" || {
    echo "attested allowed_envs differs from the compose's sealed names" >&2
    diff -u "$tmp/expected-envs" "$tmp/actual-envs" >&2 || true
    exit 1
}
violations=$(jq -r -L "$root/deploy" --arg variant "$variant" \
    'include "compose-policy"; violations($variant; "dstack")[]' "$tmp/docker-compose.json")
if [ -n "$violations" ]; then
    echo "the attested $variant compose breaks deploy/compose-policy.jq:" >&2
    printf '%s\n' "$violations" | sed 's/^/  /' >&2
    exit 1
fi
echo "attested compose, allowed_envs, and the $variant policy passed"
