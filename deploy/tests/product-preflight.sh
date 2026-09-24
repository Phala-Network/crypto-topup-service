#!/usr/bin/env bash
# Local (offline) checks of deploy/product/preflight.sh: the example env file, a malformed seed,
# and a stale render are refused; an unsealed env file is accepted only with --unsealed.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
preflight="$root/deploy/product/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export PRODUCT_IMAGE=ghcr.io/phala-network/crypto-topup-reference-product@sha256:3333333333333333333333333333333333333333333333333333333333333333
"$root/deploy/render-compose.sh" "$root/deploy/product/docker-compose.yml" >"$tmp/compose.yml"
sed 's/^\(    image: .*\)$/\1\n    init: true/' "$tmp/compose.yml" >"$tmp/stale.yml"
cat >"$tmp/unsealed.env" <<'ENV'
PRODUCT_DRIVER_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
PRODUCT_PUBLIC_URL=https://pending.invalid
PRODUCT_RPC_URL=https://rpc.example/sepolia
PRODUCT_SEED=
TOPUP_ORIGIN=https://topup.example
ENV
sed 's/^PRODUCT_SEED=$/PRODUCT_SEED=nothex/' "$tmp/unsealed.env" >"$tmp/bad-seed.env"

expect_failure() {
    local name=$1 message=$2
    shift 2
    if "$preflight" "$@" --offline >"$tmp/$name.out" 2>"$tmp/$name.err"; then
        echo "product preflight accepted $name" >&2
        exit 1
    fi
    grep -F -- "$message" "$tmp/$name.err" >/dev/null || {
        echo "product preflight rejected $name for an unexpected reason:" >&2
        cat "$tmp/$name.err" >&2
        exit 1
    }
}

expect_failure example "TOPUP_ORIGIN still contains replace-me" \
    --env "$root/deploy/product/staging.env.example" --compose "$tmp/compose.yml"
expect_failure sealed-empty "PRODUCT_SEED is empty" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml"
expect_failure bad-seed "PRODUCT_SEED must be 64 lowercase hex" --unsealed \
    --env "$tmp/bad-seed.env" --compose "$tmp/compose.yml"
expect_failure stale "differs from a fresh render" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/stale.yml"
"$preflight" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml" --offline --unsealed >/dev/null

echo "product preflight local checks test passed"
