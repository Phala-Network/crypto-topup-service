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
grep -F 'DATABASE_URL: ${DATABASE_URL:-}' "$tmp/compose.yml" >/dev/null
grep -F 'MIGRATE_DATABASE_URL: ${MIGRATE_DATABASE_URL:-}' "$tmp/compose.yml" >/dev/null

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

echo "compose image renderer test passed"
