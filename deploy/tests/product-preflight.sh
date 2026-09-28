#!/usr/bin/env bash
# Local (offline) checks of deploy/product/preflight.sh: the example env file, a malformed or live
# key, a stale render, a malformed rendered setting or account, and an RPC URL with an API key are
# refused; an unsealed env file is accepted only with --unsealed, and a restricted or secret test
# key without it.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
preflight="$root/deploy/product/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export PRODUCT_IMAGE=ghcr.io/phala-network/phala-pay-reference-product@sha256:3333333333333333333333333333333333333333333333333333333333333333
export PRODUCT_DRIVER_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
export PRODUCT_PUBLIC_URL=https://pending.invalid
export PRODUCT_RPC_URL=https://rpc.example/sepolia
export TOPUP_ORIGIN=https://topup.example
"$root/deploy/product/render-compose.sh" >"$tmp/compose.yml"
# A hand edit that keeps the label digest, e.g. of a setting, is stale.
sed 's|https://rpc.example/sepolia|https://rpc.example/other|' "$tmp/compose.yml" >"$tmp/stale.yml"
PRODUCT_RPC_URL=http://rpc.example/sepolia "$root/deploy/product/render-compose.sh" >"$tmp/http-rpc.yml"
PRODUCT_RPC_URL=https://sepolia.infura.io/v3/0123456789abcdef0123456789abcdef \
    "$root/deploy/product/render-compose.sh" >"$tmp/keyed-rpc.yml"
printf 'PRODUCT_API_KEY=\n' >"$tmp/unsealed.env"
sed 's/^PRODUCT_API_KEY=$/PRODUCT_API_KEY=sk_test_123/' "$tmp/unsealed.env" >"$tmp/bad-key.env"
key_body=$(printf 'A%.0s' {1..43})000000
printf 'PRODUCT_API_KEY=ppay_rk_live_%s\n' "$key_body" >"$tmp/live-key.env"
printf 'PRODUCT_API_KEY=ppay_rk_test_%s\n' "$key_body" >"$tmp/restricted.env"
printf 'PRODUCT_API_KEY=ppay_sk_test_%s\n' "$key_body" >"$tmp/secret.env"
# A config whose account is not an acct_ id (rendered from an edited source).
sed 's/"account": "acct_[0-9a-f]*",/"account": "phala-cloud",/' "$root/deploy/product/docker-compose.yml" \
    >"$tmp/slug-source.yml"
"$root/deploy/product/render-compose.sh" "$tmp/slug-source.yml" >"$tmp/slug.yml"

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

expect_failure example "PRODUCT_API_KEY still contains replace-me" \
    --env "$root/deploy/product/staging.env.example" --compose "$tmp/compose.yml"
expect_failure sealed-empty "PRODUCT_API_KEY is empty" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml"
expect_failure bad-key "PRODUCT_API_KEY must be a Phala Pay test API key" --unsealed \
    --env "$tmp/bad-key.env" --compose "$tmp/compose.yml"
expect_failure live-key "PRODUCT_API_KEY must be a Phala Pay test API key" \
    --env "$tmp/live-key.env" --compose "$tmp/compose.yml"
expect_failure slug "account must be the product's acct_ id" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/slug.yml"
expect_failure stale "differs from a fresh render" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/stale.yml"
expect_failure http-rpc "PRODUCT_RPC_URL must use https" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/http-rpc.yml"
expect_failure keyed-rpc "PRODUCT_RPC_URL seems to embed an API key" --unsealed \
    --env "$tmp/unsealed.env" --compose "$tmp/keyed-rpc.yml"
if grep -q 0123456789abcdef "$tmp/keyed-rpc.out" "$tmp/keyed-rpc.err"; then
    echo "product preflight printed the RPC key" >&2
    exit 1
fi
expect_failure source "the compose reads other variables" --unsealed \
    --env "$tmp/unsealed.env" --compose "$root/deploy/product/docker-compose.yml"
"$preflight" --env "$tmp/unsealed.env" --compose "$tmp/compose.yml" --offline --unsealed >/dev/null
"$preflight" --env "$tmp/restricted.env" --compose "$tmp/compose.yml" --offline >/dev/null
"$preflight" --env "$tmp/secret.env" --compose "$tmp/compose.yml" --offline >/dev/null

echo "product preflight local checks test passed"
