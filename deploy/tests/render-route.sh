#!/usr/bin/env bash
# Renders the sandbox route template from valid inputs and checks that malformed inputs are
# rejected at render time with an error naming the variable.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
render="$root/deploy/sandbox/render-route.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export FORWARDER_FACTORY=0x1111111111111111111111111111111111111111
export TEST_TOKEN=0x4444444444444444444444444444444444444444
export SANCTIONS_ORACLE=0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa
export PRODUCT_SLUG=ci-check

"$render" >"$tmp/route.yaml"
grep -F "$FORWARDER_FACTORY" "$tmp/route.yaml" >/dev/null
grep -F "$SANCTIONS_ORACLE" "$tmp/route.yaml" >/dev/null

# expect_rejection NAME=VALUE EXPECTED_ERROR: rendering with that override must fail with the error.
expect_rejection() {
    if env "$1" "$render" >"$tmp/out" 2>"$tmp/err"; then
        echo "render-route accepted $1" >&2
        exit 1
    fi
    grep -F -- "$2" "$tmp/err" >/dev/null || {
        echo "render-route rejected $1 for an unexpected reason: $(cat "$tmp/err")" >&2
        exit 1
    }
}

address_error="must be a 0x-prefixed 20-byte hex address"
expect_rejection TEST_TOKEN=0x123 "TEST_TOKEN $address_error"
expect_rejection FORWARDER_FACTORY=1111111111111111111111111111111111111111 "FORWARDER_FACTORY $address_error"
expect_rejection TEST_TOKEN=0x444444444444444444444444444444444444444g "TEST_TOKEN $address_error"
expect_rejection 'SANCTIONS_ORACLE=0x1111111111111111111111111111111111111111 ' "SANCTIONS_ORACLE $address_error"
expect_rejection FORWARDER_FACTORY= "FORWARDER_FACTORY is required"
expect_rejection PRODUCT_SLUG=Bad_Slug "PRODUCT_SLUG must be lowercase"

echo "sandbox route renderer test passed"
