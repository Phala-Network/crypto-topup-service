#!/usr/bin/env bash
# Writes the unsealed staging env file for provisioning: exactly the names of
# deploy/staging.env.example, each empty. Every name there is an owner-sealed secret, never read
# from the environment or held by GitHub; the names alone fix the CVM's allowed_envs. Public
# settings are not env values: deploy/render-compose.sh renders them into the attested compose.
# The owner later seals the secrets from their own machine (deploy/README.md).
#
# Usage: deploy/write-staging-env.sh [--product] OUTPUT
#
# --product writes the reference product's env file instead: its only name, PRODUCT_SEED
# (deploy/product/staging.env.example).
#
# OUTPUT must already exist (create it with mktemp, mode 0600); it is overwritten.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
example="$root/deploy/staging.env.example"
if [[ "${1:-}" == --product ]]; then
    example="$root/deploy/product/staging.env.example"
    shift
fi
(($# == 1)) || { echo "usage: $0 [--product] OUTPUT" >&2; exit 64; }
output=$1
[[ -f "$output" ]] || { echo "$output must exist (create it with mktemp)" >&2; exit 64; }

chmod 600 "$output"
awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, "="); print }' "$example" >"$output"
echo "wrote $(wc -l <"$output") names to $output"
