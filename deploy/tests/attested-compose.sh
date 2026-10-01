#!/usr/bin/env bash
# deploy/attested-compose.sh, which binds an attestation to the deployment's compose for Deploy's
# verify-attestation.sh and deploy.sh: it prints the SHA-256 of the attested app-compose when the
# event log names that hash once, and the app-compose carries the expected compose and the kit's
# pre-launch script byte for byte; anything else fails.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
fail() {
    echo "attested-compose: $*" >&2
    exit 1
}
printf 'services:\n  topup:\n    image: topup\n' >"$tmp/compose.yml"

# attestation COMPOSE PRE_LAUNCH [HASH...]: an attestation of the app-compose with COMPOSE and
# PRE_LAUNCH, its event log naming each HASH, or once its app-compose's own hash; on stdout.
attestation() {
    local app_compose hash
    app_compose=$(jq -cjn --rawfile compose "$1" --rawfile pre_launch "$2" \
        '{manifest_version: 2, docker_compose_file: $compose, pre_launch_script: $pre_launch}')
    hash=$(printf '%s' "$app_compose" | sha256sum | cut -d' ' -f1)
    shift 2
    (($#)) || set -- "0x${hash^^}"
    jq -n --arg app_compose "$app_compose" '{compose_file: $app_compose, tcb_info: {event_log:
        ([$ARGS.positional[] | {event: "compose-hash", event_payload: .}]
            + [{event: "instance-id", event_payload: ("a" * 40)}])}}' --args "$@"
}
# check NAME: deploy/attested-compose.sh with tmp/NAME.json and tmp/compose.yml.
check() {
    mkdir -p "$tmp/$1"
    "$root/deploy/attested-compose.sh" "$tmp/$1.json" "$tmp/compose.yml" "$tmp/$1" 2>"$tmp/$1.err"
}

pre_launch=$root/deploy/phala-cloud-pre-launch.sh
attestation "$tmp/compose.yml" "$pre_launch" >"$tmp/bound.json"
expected=$(jq -j .compose_file "$tmp/bound.json" | sha256sum | cut -d' ' -f1)
[[ "$(check bound)" == "$expected" ]] || fail "the bound attestation did not print its app-compose's hash"
cmp -s "$tmp/bound/docker-compose.yml" "$tmp/compose.yml" || fail "the attested compose was not written"

# refused NAME MESSAGE: tmp/NAME.json fails with MESSAGE.
refused() {
    ! check "$1" >/dev/null || fail "$1 was accepted"
    grep -qF -- "$2" "$tmp/$1.err" || { cat "$tmp/$1.err" >&2; fail "$1 failed for another reason"; }
}
printf 'services: {}\n' >"$tmp/other.yml"
attestation "$tmp/other.yml" "$pre_launch" >"$tmp/other-compose.json"
refused other-compose "attested docker_compose_file differs from the prepared compose"
printf 'echo other\n' >"$tmp/other-pre-launch.sh"
attestation "$tmp/compose.yml" "$tmp/other-pre-launch.sh" >"$tmp/other-pre-launch.json"
refused other-pre-launch "the attested pre_launch_script is not deploy/phala-cloud-pre-launch.sh"
attestation "$tmp/compose.yml" "$pre_launch" "$(printf 'ab%.0s' {1..32})" >"$tmp/other-hash.json"
refused other-hash "does not name compose hash $expected, its app-compose's, once"
attestation "$tmp/compose.yml" "$pre_launch" "$expected" "$expected" >"$tmp/twice.json"
refused twice "does not name compose hash $expected, its app-compose's, once"
jq 'del(.compose_file)' "$tmp/bound.json" >"$tmp/no-app-compose.json"
refused no-app-compose "the attestation carries no app-compose"
echo "attested compose test passed"
