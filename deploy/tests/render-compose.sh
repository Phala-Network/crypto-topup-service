#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

topup=ghcr.io/phala-network/crypto-topup@sha256:1111111111111111111111111111111111111111111111111111111111111111
postgres=ghcr.io/phala-network/postgres-walg@sha256:2222222222222222222222222222222222222222222222222222222222222222

TOPUP_IMAGE=$topup POSTGRES_WALG_IMAGE=$postgres \
    "$root/deploy/render-compose.sh" >"$tmp/compose.yml"

grep -F "image: $topup" "$tmp/compose.yml" >/dev/null
grep -F "image: $postgres" "$tmp/compose.yml" >/dev/null
grep -F 'TOPUP_PUBLIC_ORIGIN: ${TOPUP_PUBLIC_ORIGIN:-}' "$tmp/compose.yml" >/dev/null
grep -F 'TOPUP_SERVICE_ENABLED: ${TOPUP_SERVICE_ENABLED:-on}' "$tmp/compose.yml" >/dev/null

if TOPUP_IMAGE=crypto-topup:latest POSTGRES_WALG_IMAGE=$postgres \
    "$root/deploy/render-compose.sh" >"$tmp/bare-tag.out" 2>"$tmp/bare-tag.err"; then
    echo "render-compose accepted a bare tag" >&2
    exit 1
fi
grep -F 'TOPUP_IMAGE must be an image@sha256 reference' "$tmp/bare-tag.err" >/dev/null

if TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:0000000000000000000000000000000000000000000000000000000000000000 \
    POSTGRES_WALG_IMAGE=$postgres \
    "$root/deploy/render-compose.sh" >"$tmp/zero.out" 2>"$tmp/zero.err"; then
    echo "render-compose accepted the zero digest" >&2
    exit 1
fi
grep -F 'TOPUP_IMAGE must not use the zero digest placeholder' "$tmp/zero.err" >/dev/null

# The reference product: public settings inline, and a label digest that changes with any of
# them (Compose recreates a container only when its service definition changes).
product_env() {
    env PRODUCT_IMAGE=ghcr.io/phala-network/crypto-topup-reference-product@sha256:3333333333333333333333333333333333333333333333333333333333333333 \
        TOPUP_ORIGIN=https://topup.example PRODUCT_PUBLIC_URL=https://product.example \
        PRODUCT_RPC_URL=https://rpc.example/sepolia \
        PRODUCT_DRIVER_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo= "$@" \
        "$root/deploy/product/render-compose.sh"
}
label() {
    grep -o 'crypto-topup.rendered-sha256: "[0-9a-f]\{64\}"' "$1"
}
product_env >"$tmp/product.yml"
grep -F '"rpc_url": "https://rpc.example/sepolia",' "$tmp/product.yml" >/dev/null
product_env PRODUCT_RPC_URL=https://other-rpc.example/sepolia >"$tmp/product-rpc.yml"
[ -n "$(label "$tmp/product.yml")" ] && [ "$(label "$tmp/product.yml")" != "$(label "$tmp/product-rpc.yml")" ] || {
    echo "the product label digest does not change with PRODUCT_RPC_URL" >&2
    exit 1
}
if product_env 'PRODUCT_RPC_URL=https://rpc.example/$SECRET' >"$tmp/dollar.out" 2>"$tmp/dollar.err"; then
    echo "the product renderer accepted a value with \$" >&2
    exit 1
fi
grep -F 'PRODUCT_RPC_URL must be' "$tmp/dollar.err" >/dev/null
if grep -F 'rpc.example' "$tmp/dollar.err" >/dev/null; then
    echo "the product renderer printed a rejected value" >&2
    exit 1
fi
if product_env TOPUP_ORIGIN= >/dev/null 2>"$tmp/empty.err"; then
    echo "the product renderer accepted an empty TOPUP_ORIGIN" >&2
    exit 1
fi
grep -F 'TOPUP_ORIGIN must be' "$tmp/empty.err" >/dev/null

echo "compose image renderer test passed"
