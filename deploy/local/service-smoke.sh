#!/usr/bin/env bash
set -euo pipefail

if [[ "${SERVICE_SMOKE:-0}" != "1" ]]; then
    echo "service smoke not requested; set SERVICE_SMOKE=1 after the unified run command is available"
    exit 0
fi

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
project="topup-service-smoke-$$"
tmp=$(mktemp -d)
port=${TOPUP_LOCAL_PORT:-18080}

cleanup() {
    docker compose -p "$project" -f "$compose" down --volumes --remove-orphans
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

mkdir -p "$tmp/routes"
sed 's/0x0000000000000000000000000000000000000000/0x3333333333333333333333333333333333333333/g' \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" \
    >"$tmp/routes/phala-cloud-sepolia-pha.yaml"

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
export TOPUP_LOCAL_ROUTES_DIR="$tmp/routes"

docker compose -p "$project" -f "$compose" build postgres dstack-simulator topup
run_help=$(docker compose -p "$project" -f "$compose" run --rm --no-deps topup \
    topup run --help)
printf '%s\n' "$run_help"
printf '%s\n' "$run_help" | grep -F -- '--bind' >/dev/null || {
    echo "unified topup run command is not available" >&2
    exit 1
}
printf '%s\n' "$run_help" | grep -F -- '--route' >/dev/null

docker compose -p "$project" -f "$compose" up -d postgres dstack-simulator backup topup

attempts=60
while ! timeout 1 bash -c "</dev/tcp/127.0.0.1/$port" 2>/dev/null; do
    attempts=$((attempts - 1))
    if [[ "$attempts" -eq 0 ]]; then
        echo "timed out waiting for topup TCP port $port" >&2
        docker compose -p "$project" -f "$compose" logs topup >&2
        exit 1
    fi
    sleep 2
done
echo "topup TCP listener passed"

curl -fsS "http://127.0.0.1:$port/openapi.json" | jq -e '.openapi and .info.title' >/dev/null
echo "GET /openapi.json passed"
echo "service smoke passed"
