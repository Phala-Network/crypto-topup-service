#!/bin/sh
set -eu

# Binds an attestation to this deployment's compose, and prints the attested compose hash: the
# SHA-256 (lowercase hex) of the attestation's app-compose, the full app-compose JSON the Phala CLI
# built. The event log's compose-hash must be that hash, the app-compose's docker_compose_file
# EXPECTED_COMPOSE byte for byte, and its pre_launch_script deploy/phala-cloud-pre-launch.sh byte for
# byte. Writes the app-compose to OUT_DIR/app-compose.json and its docker_compose_file to
# OUT_DIR/docker-compose.yml. It reads the event log as reported: verify-attestation.sh replays it
# against the quote with the dstack verifier, and compares that replay with this hash.
#
# Usage: deploy/attested-compose.sh ATTESTATION_JSON EXPECTED_COMPOSE OUT_DIR
if [ "$#" -ne 3 ]; then
    echo "usage: attested-compose.sh ATTESTATION_JSON EXPECTED_COMPOSE OUT_DIR" >&2
    exit 64
fi
root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
attestation=$1
expected_compose=$2
out=$3

jq -jer '.compose_file | select(type == "string" and length > 0)' "$attestation" >"$out/app-compose.json" || {
    echo "the attestation carries no app-compose" >&2
    exit 1
}
if command -v sha256sum >/dev/null; then
    compose_hash=$(sha256sum "$out/app-compose.json" | awk '{print $1}')
else
    compose_hash=$(shasum -a 256 "$out/app-compose.json" | awk '{print $1}')
fi
jq -e --arg hash "$compose_hash" '[.tcb_info.event_log[]? | select(.event == "compose-hash")
    | .event_payload | ascii_downcase | ltrimstr("0x")] == [$hash]' "$attestation" >/dev/null || {
    echo "the attestation's event log does not name compose hash $compose_hash, its app-compose's, once" >&2
    exit 1
}

jq -e 'type == "object"' "$out/app-compose.json" >/dev/null
jq -j '.pre_launch_script // ""' "$out/app-compose.json" >"$out/pre-launch.sh"
cmp -s "$out/pre-launch.sh" "$root/deploy/phala-cloud-pre-launch.sh" || {
    echo "the attested pre_launch_script is not deploy/phala-cloud-pre-launch.sh" >&2
    exit 1
}
jq -jer '.docker_compose_file | select(type == "string" and length > 0)' \
    "$out/app-compose.json" >"$out/docker-compose.yml"
cmp -s "$expected_compose" "$out/docker-compose.yml" || {
    echo "attested docker_compose_file differs from the prepared compose" >&2
    diff -u "$expected_compose" "$out/docker-compose.yml" >&2 || true
    exit 1
}
printf '%s\n' "$compose_hash"
