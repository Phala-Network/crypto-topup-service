#!/usr/bin/env bash
# Renders the sandbox route template from valid inputs and checks that malformed inputs are
# rejected at render time with an error naming the variable.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
render="$root/deploy/sandbox/render-route.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export FORWARDER_FACTORY=0x1111111111111111111111111111111111111111
export IMPLEMENTATION=0x2222222222222222222222222222222222222222
export TREASURY=0x3333333333333333333333333333333333333333
export TEST_TOKEN=0x4444444444444444444444444444444444444444
export SANCTIONS_ORACLE=0xaAaAaAaaAaAaAaaAaAAAAAAAAaaaAaAaAaaAaaAa
export PRODUCT_SLUG=ci-check PRODUCT_KID=ci-check/v1
export SETTLEMENT_URL=https://product.invalid/settlements

"$render" >"$tmp/route.yaml"
grep -F "$TREASURY" "$tmp/route.yaml" >/dev/null
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
expect_rejection TREASURY=0x123 "TREASURY $address_error"
expect_rejection FORWARDER_FACTORY=1111111111111111111111111111111111111111 "FORWARDER_FACTORY $address_error"
expect_rejection IMPLEMENTATION=0x22222222222222222222222222222222222222222 "IMPLEMENTATION $address_error"
expect_rejection TEST_TOKEN=0x444444444444444444444444444444444444444g "TEST_TOKEN $address_error"
expect_rejection 'SANCTIONS_ORACLE=0x1111111111111111111111111111111111111111 ' "SANCTIONS_ORACLE $address_error"
expect_rejection TREASURY= "TREASURY is required"
expect_rejection PRODUCT_SLUG=Bad_Slug "PRODUCT_SLUG must be lowercase"
expect_rejection 'SETTLEMENT_URL=https://x.invalid/$HOME' "SETTLEMENT_URL must be printable ASCII"

echo "sandbox route renderer test passed"
