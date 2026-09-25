#!/usr/bin/env bash
# Local (offline) checks of deploy/product/preflight.sh: the example env file, a malformed seed,
# a stale render, and a malformed rendered setting are refused; an unsealed env file is accepted
# only with --unsealed.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
preflight="$root/deploy/product/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export PRODUCT_IMAGE=ghcr.io/phala-network/crypto-topup-reference-product@sha256:3333333333333333333333333333333333333333333333333333333333333333
export PRODUCT_DRIVER_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
export PRODUCT_PUBLIC_URL=https://pending.invalid
export PRODUCT_RPC_URL=https://rpc.example/sepolia
export TOPUP_ORIGIN=https://topup.example
"$root/deploy/product/render-compose.sh" >"$tmp/compose.yml"
# A hand edit that keeps the label digest, e.g. of a setting, is stale.
sed 's|https://rpc.example/sepolia|https://rpc.example/other|' "$tmp/compose.yml" >"$tmp/stale.yml"
PRODUCT_RPC_URL=http://rpc.example/sepolia "$root/deploy/product/render-compose.sh" >"$tmp/http-rpc.yml"
printf 'PRODUCT_SEED=\n' >"$tmp/unsealed.env"
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

expect_failure example "PRODUCT_SEED still contains replace-me" \
    --env "$root/deploy/product/staging.env.example" --compose "$tmp/compose.yml"
expect_failure sealed-empty "PRODUCT_SEED is empty" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml"
expect_failure bad-seed "PRODUCT_SEED must be 64 lowercase hex" --unsealed \
    --env "$tmp/bad-seed.env" --compose "$tmp/compose.yml"
expect_failure stale "differs from a fresh render" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/stale.yml"
expect_failure http-rpc "PRODUCT_RPC_URL must use https" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/http-rpc.yml"
expect_failure source "the compose reads other variables" --unsealed \
    --env "$tmp/unsealed.env" --compose "$root/deploy/product/docker-compose.yml"
"$preflight" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml" --offline --unsealed >/dev/null

echo "product preflight local checks test passed"
