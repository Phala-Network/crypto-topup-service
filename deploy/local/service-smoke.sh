#!/usr/bin/env bash
set -euo pipefail

if [[ "${SERVICE_SMOKE:-0}" != "1" ]]; then
    echo "service smoke not requested; set SERVICE_SMOKE=1 to run the service checks"
    exit 0
fi

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq; do
    require_command "$command"
done
# `topup run` refuses to start until the route's contracts are verified on chain, so the smoke
# runs against the sandbox overlay's Anvil chain with a real factory deployment.
compose="$root/deploy/local/docker-compose.yml"
sandbox_compose="$root/deploy/sandbox/docker-compose.local.yml"
project="topup-service-smoke-$$"
tmp=$(mktemp -d)
port=${TOPUP_LOCAL_PORT:-18080}
export SANDBOX_ANVIL_PORT=${SANDBOX_ANVIL_PORT:-18545}
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
export TOPUP_LOCAL_DSTACK_IMAGE="$project-dstack"
export TOPUP_LOCAL_POSTGRES_IMAGE="$project-postgres"
export TOPUP_LOCAL_SERVICE_IMAGE="$project-topup"

cleanup() {
    docker compose -p "$project" -f "$compose" -f "$sandbox_compose" down --volumes --remove-orphans
    docker image rm "$TOPUP_LOCAL_DSTACK_IMAGE" "$TOPUP_LOCAL_POSTGRES_IMAGE" \
        "$TOPUP_LOCAL_SERVICE_IMAGE" >/dev/null 2>&1 || true
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM

wait_for() {
    description=$1
    shift
    attempts=60
    while [[ "$attempts" -gt 0 ]]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 2
    done
    echo "timed out waiting for $description" >&2
    docker compose -p "$project" -f "$compose" -f "$sandbox_compose" logs >&2
    return 1
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
export TOPUP_LOCAL_ROUTES_DIR="$tmp/routes"
mkdir -p "$TOPUP_LOCAL_ROUTES_DIR"

docker compose -p "$project" -f "$compose" -f "$sandbox_compose" build postgres dstack-simulator topup
docker compose -p "$project" -f "$compose" -f "$sandbox_compose" up -d anvil
wait_for anvil cast chain-id --rpc-url "$rpc_url"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
(cd "$root/contracts" && ADMIN="$owner" TREASURY="$owner" FOUNDRY_BROADCAST="$tmp/broadcast" \
    forge script script/DeployFactory.s.sol:DeployFactory --rpc-url "$rpc_url" \
    --private-key "$ANVIL_PRIVATE_KEY" --broadcast --silent)
factory=$(predicted_factory "$owner" "$owner")
implementation=$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")
# The sandbox overlay runs `topup run --route /etc/topup/routes/sandbox.yaml`.
sed -e "s|^    forwarder_factory: .*|    forwarder_factory: \"$factory\"|" \
    -e "s|^    implementation: .*|    implementation: \"$implementation\"|" \
    -e "s|^    treasury: .*|    treasury: \"$owner\"|" \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" >"$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
chmod 0644 "$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
run_help=$(docker compose -p "$project" -f "$compose" -f "$sandbox_compose" run --rm --no-deps topup \
    topup run --help)
printf '%s\n' "$run_help"
printf '%s\n' "$run_help" | grep -F -- '--bind' >/dev/null || {
    echo "unified topup run command is not available" >&2
    exit 1
}
printf '%s\n' "$run_help" | grep -F -- '--metrics-bind' >/dev/null
printf '%s\n' "$run_help" | grep -F -- '--route' >/dev/null

docker compose -p "$project" -f "$compose" -f "$sandbox_compose" up -d postgres dstack-simulator backup topup

wait_for "topup TCP port $port" timeout 1 bash -c "</dev/tcp/127.0.0.1/$port"
echo "topup TCP listener passed"

wait_for "GET /healthz 200" curl -fsS "http://127.0.0.1:$port/healthz"
status=$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/healthz")
[[ "$status" == "200" ]]
echo "GET /healthz returned 200"

curl -fsS "http://127.0.0.1:$port/openapi.json" | jq -e '.openapi and .info.title' >/dev/null
echo "GET /openapi.json passed"

wait_for backup docker compose -p "$project" -f "$compose" -f "$sandbox_compose" \
    exec -T backup wal-g --version
echo "local backup service is running"
echo "service smoke passed"
