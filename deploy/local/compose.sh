#!/usr/bin/env bash
# `docker compose` for the local stacks (make up, smokes, sandbox, restore drill): the attested
# compose rendered by deploy/render-compose.sh exactly as for a CVM, with local settings, then the
# local overlay (deploy/local/docker-compose.yml) and any further `-f` files the caller passes.
# The overlay builds the images from this checkout, so the rendered digests are placeholders, and
# it supplies the local object-storage credentials and ports. --restore-check renders the restore
# verification variant (deploy/RESTORE.md), as the restore drill's replacement boots it.
#
# Local settings come from TOPUP_LOCAL_* (sandbox and drill drivers), and TOPUP_BACKUP_KEY_VERSION
# and TOPUP_BACKUP_KEY_FALLBACK_VERSIONS (the drill's key rotation); nothing else is read from the
# shell, so a developer's AWS_* variables never reach the local stack.
#
# Usage: deploy/local/compose.sh [--restore-check] [COMPOSE OPTIONS] COMMAND [ARGS...]
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
variant=()
if [[ "${1:-}" == --restore-check ]]; then
    variant=(--restore-check)
    shift
fi
rendered=$(mktemp "${TMPDIR:-/tmp}/topup-local-compose.XXXXXX")
trap 'rm -f "$rendered"' EXIT
env -i PATH="$PATH" \
    TOPUP_IMAGE=crypto-topup-service-local@sha256:1111111111111111111111111111111111111111111111111111111111111111 \
    POSTGRES_WALG_IMAGE=crypto-topup-postgres-walg-local@sha256:2222222222222222222222222222222222222222222222222222222222222222 \
    WALG_S3_PREFIX=s3://topup-backups/postgres AWS_ENDPOINT=http://s3:3900 AWS_REGION=us-east-1 \
    AWS_S3_FORCE_PATH_STYLE=true \
    TOPUP_BACKUP_KEY_VERSION="${TOPUP_BACKUP_KEY_VERSION:-1}" \
    TOPUP_BACKUP_KEY_FALLBACK_VERSIONS="${TOPUP_BACKUP_KEY_FALLBACK_VERSIONS:-0}" \
    TOPUP_ADMIN_KID="${TOPUP_LOCAL_ADMIN_KID:-local-admin/v1}" \
    TOPUP_ADMIN_PUBLIC_KEY="${TOPUP_LOCAL_ADMIN_PUBLIC_KEY:-11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=}" \
    TOPUP_PUBLIC_ORIGIN="http://127.0.0.1:${TOPUP_LOCAL_PORT:-18080}" \
    TOPUP_RPC_PROVIDER_A_URL=http://127.0.0.1:1 TOPUP_RPC_PROVIDER_B_URL=http://127.0.0.1:1 \
    "$root/deploy/render-compose.sh" "${variant[@]}" >"$rendered"
# Relative paths in the overlays resolve against deploy/, as for deploy/docker-compose.yml itself.
docker compose --project-directory "$root/deploy" -f "$rendered" \
    -f "$root/deploy/local/docker-compose.yml" "$@"
