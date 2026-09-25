#!/usr/bin/env bash
# render-compose.sh: image digests, attested settings, the two variants, the rendered-sha256 label,
# and the app-compose preview; the product renderer on the same path.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

topup=ghcr.io/phala-network/crypto-topup@sha256:1111111111111111111111111111111111111111111111111111111111111111
postgres=ghcr.io/phala-network/postgres-walg@sha256:2222222222222222222222222222222222222222222222222222222222222222

settings=(AWS_ENDPOINT=https://account.r2.cloudflarestorage.com AWS_REGION=auto
    AWS_S3_FORCE_PATH_STYLE=false WALG_S3_PREFIX=s3://topup-staging/postgres
    TOPUP_ADMIN_KID=staging-admin/v1 TOPUP_ADMIN_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
    TOPUP_BACKUP_KEY_VERSION=2 TOPUP_BACKUP_KEY_FALLBACK_VERSIONS=1,0
    TOPUP_PUBLIC_ORIGIN=https://topup.example
    TOPUP_RPC_PROVIDER_A_URL=https://rpc-a.example/sepolia
    TOPUP_RPC_PROVIDER_B_URL=https://rpc-b.example/sepolia)
render() {
    env TOPUP_IMAGE=$topup POSTGRES_WALG_IMAGE=$postgres "${settings[@]}" "$@"
}
label() {
    grep -o 'crypto-topup.rendered-sha256: "[0-9a-f]\{64\}"' "$1" | sort -u
}
render "$root/deploy/render-compose.sh" >"$tmp/compose.yml"

grep -F "image: $topup" "$tmp/compose.yml" >/dev/null
grep -F "image: $postgres" "$tmp/compose.yml" >/dev/null
grep -F 'TOPUP_PUBLIC_ORIGIN: "https://topup.example"' "$tmp/compose.yml" >/dev/null
grep -F 'TOPUP_SERVICE_ENABLED: "on"' "$tmp/compose.yml" >/dev/null
grep -F 'TOPUP_RESTORE_FROM_BACKUP: "off"' "$tmp/compose.yml" >/dev/null
grep -F -- '- "1,0"' "$tmp/compose.yml" >/dev/null
# Only the owner-sealed secrets stay env references.
docker compose -f "$tmp/compose.yml" config --variables | awk 'NR > 1 && NF > 0 { print $1 }' |
    sort |
    diff -u <(awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' \
        "$root/deploy/staging.env.example" | sort) -
# Every service carries the one label, and it changes with any setting.
[[ "$(docker compose -f "$tmp/compose.yml" --profile tools config --format json |
    jq '[.services[].labels["crypto-topup.rendered-sha256"]] | unique | length')" == 1 ]]
render TOPUP_RPC_PROVIDER_A_URL=https://other.example/sepolia "$root/deploy/render-compose.sh" \
    >"$tmp/other-rpc.yml"
[[ -n "$(label "$tmp/compose.yml")" && "$(label "$tmp/compose.yml")" != "$(label "$tmp/other-rpc.yml")" ]] || {
    echo "the label digest does not change with TOPUP_RPC_PROVIDER_A_URL" >&2
    exit 1
}
# Modes come only from the variant, never from the environment.
render TOPUP_SERVICE_ENABLED=off TOPUP_RESTORE_FROM_BACKUP=on "$root/deploy/render-compose.sh" |
    cmp -s - "$tmp/compose.yml" || {
    echo "the environment changed a mode switch" >&2
    exit 1
}
render "$root/deploy/render-compose.sh" --restore-check >"$tmp/restore-check.yml"
grep -F 'TOPUP_SERVICE_ENABLED: "read-only"' "$tmp/restore-check.yml" >/dev/null
grep -F 'TOPUP_RESTORE_FROM_BACKUP: "on"' "$tmp/restore-check.yml" >/dev/null
[[ "$(label "$tmp/compose.yml")" != "$(label "$tmp/restore-check.yml")" ]]
if render TOPUP_ADMIN_KID= "$root/deploy/render-compose.sh" >/dev/null 2>"$tmp/empty.err"; then
    echo "render-compose accepted an empty setting" >&2
    exit 1
fi
grep -F 'TOPUP_ADMIN_KID must be' "$tmp/empty.err" >/dev/null
if render 'WALG_S3_PREFIX=s3://secret"bucket' "$root/deploy/render-compose.sh" >/dev/null 2>"$tmp/quote.err"; then
    echo "render-compose accepted a quote" >&2
    exit 1
fi
if grep -F 'secret' "$tmp/quote.err" >/dev/null; then
    echo "render-compose printed a rejected value" >&2
    exit 1
fi
# The app-compose preview carries the rendered compose and the secrets as allowed_envs.
render "$root/deploy/render-app-compose.sh" | jq -e --rawfile compose "$tmp/compose.yml" \
    '.docker_compose_file == $compose
    and .allowed_envs == ["AWS_ACCESS_KEY_ID", "AWS_SECRET_ACCESS_KEY", "SENTRY_DSN"]' >/dev/null
# --images-only (Release images) needs no settings.
TOPUP_IMAGE=$topup POSTGRES_WALG_IMAGE=$postgres "$root/deploy/render-compose.sh" --images-only |
    grep -F "image: $topup" >/dev/null

if render TOPUP_IMAGE=crypto-topup:latest \
    "$root/deploy/render-compose.sh" >"$tmp/bare-tag.out" 2>"$tmp/bare-tag.err"; then
    echo "render-compose accepted a bare tag" >&2
    exit 1
fi
grep -F 'TOPUP_IMAGE must be an image@sha256 reference' "$tmp/bare-tag.err" >/dev/null

if render TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:0000000000000000000000000000000000000000000000000000000000000000 \
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
product_env >"$tmp/product.yml"
grep -F '"rpc_url": "https://rpc.example/sepolia",' "$tmp/product.yml" >/dev/null
product_env PRODUCT_RPC_URL=https://other-rpc.example/sepolia >"$tmp/product-rpc.yml"
[[ -n "$(label "$tmp/product.yml")" && "$(label "$tmp/product.yml")" != "$(label "$tmp/product-rpc.yml")" ]] || {
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

echo "compose renderer test passed"
