#!/usr/bin/env bash
# Renders the Sepolia sandbox compose: deploy/docker-compose.yml with literal image digests and
# the attested settings from the environment (deploy/render-compose.sh), merged with
# docker-compose.sepolia.yml, and the validated sandbox route inlined as the `topup_route_sandbox`
# config. Output is compose JSON on stdout; secret `${NAME:-}` references stay uninterpolated for
# the CVM's encrypted environment.
#
# Usage: TOPUP_IMAGE=...@sha256:... POSTGRES_WALG_IMAGE=...@sha256:... <settings> \
#   render-sepolia-compose.sh ROUTE_FILE > sandbox-compose.json
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
route=${1:?usage: $0 ROUTE_FILE}
[[ -f "$route" ]] || { echo "route file not found: $route" >&2; exit 2; }
if grep -q '0x0000000000000000000000000000000000000000\|\${' "$route"; then
    echo "route still contains placeholders; render it with render-route.sh" >&2
    exit 2
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
"$root/deploy/render-compose.sh" >"$tmp/docker-compose.yml"
cp "$root/deploy/sandbox/docker-compose.sepolia.yml" "$tmp/docker-compose.sepolia.yml"
docker compose --project-name phala-pay-sandbox \
    -f "$tmp/docker-compose.yml" -f "$tmp/docker-compose.sepolia.yml" \
    config --no-interpolate --format json |
    jq --rawfile route "$route" '
        .configs.topup_route_sandbox = {content: $route}
        | .services.topup.configs |= map(select(.source != "topup_route_phala_cloud_sepolia_pha"))
        | del(.configs.topup_route_phala_cloud_sepolia_pha)
    '
