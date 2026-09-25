#!/usr/bin/env bash
set -euo pipefail

if [[ "${SERVICE_SMOKE:-0}" != "1" ]]; then
    echo "service smoke not requested; set SERVICE_SMOKE=1 to run the service checks"
    exit 0
fi

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq python3; do
    require_command "$command"
done
# `topup run` refuses to start until the route's contracts are verified on chain, so the smoke
# runs against the sandbox overlay's Anvil chain with a real factory deployment.
sandbox_compose="$root/deploy/sandbox/docker-compose.local.yml"
project="topup-service-smoke-$$"
tmp=$(mktemp -d)
# Free host ports by default, so the smoke can run beside a drill or a sandbox run.
export TOPUP_LOCAL_PORT=${TOPUP_LOCAL_PORT:-$(free_port)}
export SANDBOX_ANVIL_PORT=${SANDBOX_ANVIL_PORT:-$(free_port)}
port=$TOPUP_LOCAL_PORT
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
export TOPUP_LOCAL_DSTACK_IMAGE="$project-dstack"
export TOPUP_LOCAL_POSTGRES_IMAGE="$project-postgres"
export TOPUP_LOCAL_SERVICE_IMAGE="$project-topup"

dc() {
    "$root/deploy/local/compose.sh" -p "$project" -f "$sandbox_compose" "$@"
}

cleanup() {
    dc down --volumes --remove-orphans
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
    dc logs >&2
    return 1
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
export TOPUP_LOCAL_ROUTES_DIR="$tmp/routes"
mkdir -p "$TOPUP_LOCAL_ROUTES_DIR"

dc build postgres dstack-simulator topup
dc up -d anvil
wait_for anvil cast chain-id --rpc-url "$rpc_url"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
(cd "$root/contracts" && ADMIN="$owner" TREASURY="$owner" FOUNDRY_BROADCAST="$tmp/broadcast" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" forge script script/DeployFactory.s.sol:DeployFactory \
    --rpc-url "$rpc_url" --broadcast --silent)
factory=$(predicted_factory "$owner" "$owner")
implementation=$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")
# The sandbox overlay runs `topup run --route /etc/topup/routes/sandbox.yaml`.
sed -e "s|^    forwarder_factory: .*|    forwarder_factory: \"$factory\"|" \
    -e "s|^    implementation: .*|    implementation: \"$implementation\"|" \
    -e "s|^    treasury: .*|    treasury: \"$owner\"|" \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" >"$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
chmod 0644 "$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
run_help=$(dc run --rm --no-deps topup topup run --help)
printf '%s\n' "$run_help"
printf '%s\n' "$run_help" | grep -F -- '--bind' >/dev/null || {
    echo "unified topup run command is not available" >&2
    exit 1
}
printf '%s\n' "$run_help" | grep -F -- '--metrics-bind' >/dev/null
printf '%s\n' "$run_help" | grep -F -- '--route' >/dev/null

dc up -d postgres dstack-simulator backup topup

wait_for "topup TCP port $port" timeout 1 bash -c "</dev/tcp/127.0.0.1/$port"
echo "topup TCP listener passed"

wait_for "GET /healthz 200" curl -fsS "http://127.0.0.1:$port/healthz"
status=$(curl -sS -o /dev/null -w '%{http_code}' "http://127.0.0.1:$port/healthz")
[[ "$status" == "200" ]]
echo "GET /healthz returned 200"

curl -fsS "http://127.0.0.1:$port/openapi.json" | jq -e '.openapi and .info.title' >/dev/null
echo "GET /openapi.json passed"

wait_for backup dc exec -T backup wal-g --version
echo "local backup service is running"
echo "service smoke passed"
