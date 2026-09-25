#!/bin/sh
set -eu

# Runs the official dstack verifier of the pinned dstack release on the verification request on
# stdin and prints its JSON result; exits non-zero unless the result is valid. The request is
# `{"quote": null, "attestation": HEX}` for a versioned attestation (the dstack guest agent's
# Attest, as /v1/attestation returns it), or `{"attestation": null, "quote": HEX, "event_log":
# JSON, "vm_config": JSON}`. The verifier checks the TDX quote and its TCB with Intel's collateral,
# replays the event log against RTMR3, and recomputes the OS image measurements from the
# vm_config's os_image_hash (downloaded from download.dstack.org).
#
# dstacktee/dstack-verifier:0.5.9 is built from dstack 282eeb27 (its /etc/.GIT_REV), the v0.5.9
# tag that deploy/local/Dockerfile.dstack-simulator pins and the dstack-0.5.9 OS image runs. Its
# one-shot mode reads a file and writes the result next to it, so the request and result pass
# through the container's stdin and stdout; nothing is bind-mounted.
if [ "$#" -ne 0 ]; then
    echo "usage: dstack-verifier.sh <REQUEST_JSON >RESULT_JSON" >&2
    exit 64
fi

image=dstacktee/dstack-verifier:0.5.9@sha256:cfc06d5bdaa71a8a942c8bfa04d2d17dc30f13d92f26386c5d45d454606e8b70

result=$(docker run --rm -i --entrypoint sh "$image" -c '
    cat >/tmp/request.json &&
        { dstack-verifier --config /etc/dstack/dstack-verifier.toml --verify /tmp/request.json >&2 || :; } &&
        cat /tmp/request.json.verification.json')
printf '%s\n' "$result"
printf '%s' "$result" | jq -e '.is_valid == true' >/dev/null || {
    echo "dstack verifier: $(printf '%s' "$result" | jq -r '.reason // "invalid"')" >&2
    exit 1
}
