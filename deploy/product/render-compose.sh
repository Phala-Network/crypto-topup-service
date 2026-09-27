#!/usr/bin/env bash
# Renders the reference-product compose for a CVM (deploy/README.md, "Staging reference product")
# with deploy/render-compose.sh: PRODUCT_IMAGE to a digest, the public settings (TOPUP_ORIGIN,
# PRODUCT_PUBLIC_URL, PRODUCT_RPC_URL, PRODUCT_DRIVER_PUBLIC_KEY) inline from the environment, and
# the label phala-pay.rendered-sha256 to the digest of the rendered file. The output reads only
# PRODUCT_SEED (deploy/product/staging.env.example) from the env.
#
# Usage: deploy/product/render-compose.sh [SOURCE_COMPOSE] >docker-compose.product.yml
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
exec "$root/deploy/render-compose.sh" --env-example "$root/deploy/product/staging.env.example" \
    "${1:-"$root/deploy/product/docker-compose.yml"}"
