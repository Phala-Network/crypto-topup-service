#!/usr/bin/env bash
# Runs the Python SDK example, the reference product (deploy/product), and the sandbox scenarios
# against a disposable local stack: the attested compose with the deploy/local overlay plus an Anvil
# chain (docker-compose.local.yml). Everything it starts is removed on exit.
# Usage: deploy/sandbox/run-local.sh [SCENARIO ...]
#
# Requires docker compose, Foundry (forge, cast), jq, uv, curl, and OpenSSL 3. Prices come from the
# live Coin Metrics, Binance, and Kraken endpoints, exactly as on Sepolia.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq uv python3 curl openssl; do
    require_command "$command"
done

project="topup-sandbox-$$"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-sandbox.XXXXXX")
compose=("$root/deploy/local/compose.sh" -p "$project" -f "$root/deploy/sandbox/docker-compose.local.yml")
# The product side (example, reference product, and scenarios) runs in this image on the compose
# network, so the service reaches its endpoints as http://product:8089 even where a host firewall
# drops traffic from containers to the host.
client_image="ghcr.io/astral-sh/uv:0.12.18-python3.14-trixie-slim@sha256:00facf17b58b02b725155862c5cd637f688f906bf7eb5b5194647886d8805cf3"
client="$project-product"

# shellcheck disable=SC2329  # invoked by the trap
cleanup() {
    status=$?
    if ((status != 0)); then
        echo "--- topup logs (last 80 lines) ---" >&2
        "${compose[@]}" logs --no-color --tail 80 topup >&2 || true
    fi
    docker rm -f "$client" >/dev/null 2>&1 || true
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || true
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT INT TERM

wait_for() {
    local description=$1 attempts=90
    shift
    until "$@" >/dev/null 2>&1; do
        attempts=$((attempts - 1))
        ((attempts > 0)) || { echo "timed out waiting for $description" >&2; return 1; }
        sleep 2
    done
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
TOPUP_LOCAL_PORT=$(free_port)
SANDBOX_ANVIL_PORT=$(free_port)
export TOPUP_LOCAL_PORT SANDBOX_ANVIL_PORT
export TOPUP_LOCAL_ROUTES_DIR="$tmp/routes"
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
service_url="http://127.0.0.1:$TOPUP_LOCAL_PORT"
public_url="http://product:8089"
slug="sandbox-local"
mkdir -p "$TOPUP_LOCAL_ROUTES_DIR"
# A throwaway admin key for this run; the service issues the product through the admin API.
export TOPUP_LOCAL_ADMIN_KID="sandbox-admin/v1"
openssl genpkey -algorithm ed25519 -out "$tmp/admin.pem"
TOPUP_LOCAL_ADMIN_PUBLIC_KEY=$(openssl pkey -in "$tmp/admin.pem" -pubout -outform DER |
    tail -c 32 | base64)
export TOPUP_LOCAL_ADMIN_PUBLIC_KEY

echo "== building and starting postgres, dstack simulator, and anvil"
"${compose[@]}" build postgres dstack-simulator topup
"${compose[@]}" up -d postgres dstack-simulator anvil
wait_for anvil cast chain-id --rpc-url "$rpc_url"
install_anvil_multicall3 "$rpc_url"
"${compose[@]}" run --rm migrate >"$tmp/migrate.log" 2>&1 || { cat "$tmp/migrate.log" >&2; exit 1; }

echo "== deploying the forwarder factory and sandbox test contracts"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
(cd "$root/contracts" && FOUNDRY_BROADCAST="$tmp/broadcast" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" forge script script/DeployFactory.s.sol:DeployFactory \
    --rpc-url "$rpc_url" --broadcast --silent)
factory=$(predicted_factory)
implementation=$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")
"$root/deploy/sandbox/deploy-test-contracts.sh" --anvil-unlocked "$owner" \
    --rpc-url "$rpc_url" >"$tmp/contracts.json"
jq . "$tmp/contracts.json"

echo "== rendering the sandbox route"
FORWARDER_FACTORY="$factory" TREASURY="$owner" \
    TEST_TOKEN=$(jq -er .test_token "$tmp/contracts.json") \
    SANCTIONS_ORACLE=$(jq -er .sanctions_oracle "$tmp/contracts.json") \
    PRODUCT_SLUG="$slug" \
    RATE_LOCK_WINDOW_S=45 \
    "$root/deploy/sandbox/render-route.sh" >"$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
"${compose[@]}" run --rm --no-deps topup topup route validate /etc/topup/routes/sandbox.yaml

echo "== starting the service"
"${compose[@]}" up -d topup
wait_for "GET /healthz" curl -fsS "$service_url/healthz"

echo "== creating the sandbox account through POST /v1/admin/accounts"
# Signed for the service's public origin (http://topup:8080), sent to its published port. The
# response carries the account's first test key, shown once.
jq -n --arg name "$slug" --arg webhook_url "$public_url/webhooks" --arg today "$(date -u +%F)" \
    '{name: $name, contact: {name: "Sandbox", email: "sandbox@example.com"},
      due_diligence: {reference: "sandbox", reviewed_at: $today, reviewed_by: "run-local.sh"},
      charges_enabled: false, reason: "local sandbox", webhook_url: $webhook_url}' \
    >"$tmp/product.json"
mapfile -t headers < <("$root/deploy/runbooks/sign-admin-request.sh" POST \
    http://topup:8080/v1/admin/accounts "$tmp/product.json" "$tmp/admin.pem" \
    "$TOPUP_LOCAL_ADMIN_KID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' \
    -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
    --data-binary @"$tmp/product.json" "$service_url/v1/admin/accounts" >"$tmp/account.json"
account=$(jq -er .id "$tmp/account.json")
(umask 077 && jq -jer '.api_keys[0].secret' "$tmp/account.json" >"$tmp/product.key")
echo "created $account"

# Addresses as seen from the product container on the compose network.
jq -n \
    --arg account "$account" --arg route "sandbox-$slug-tpha-usd" \
    --arg factory "$factory" --arg implementation "$implementation" \
    --arg token "$(jq -er .test_token "$tmp/contracts.json")" \
    --arg unsupported "$(jq -er .unsupported_token "$tmp/contracts.json")" \
    --arg public_url "$public_url" --arg payer "$owner" --arg topup "$project-topup-1" \
    '{service_url: "http://topup:8080", product_slug: $account,
      api_key_file: "/sandbox/product.key", route: $route, chain_id: 11155111,
      rpc_url: "http://anvil:8545", factory: $factory, implementation: $implementation,
      treasury: $payer, token: $token, token_symbol: "PHA", unsupported_token: $unsupported,
      listen_host: "0.0.0.0", listen_port: 8089, public_url: $public_url, payer: $payer,
      restart_command: ["python", "/repo/deploy/sandbox/scenarios/docker_restart.py", $topup]}' \
    >"$tmp/sandbox.json"
mkdir -p "$tmp/home"

# Runs a repository Python script in the product container on the compose network.
run_product() {
    local socket=()
    if [[ "$1" == --docker-socket ]]; then
        socket=(--group-add "$(stat -c %g /var/run/docker.sock)"
            -v /var/run/docker.sock:/var/run/docker.sock)
        shift
    fi
    docker run --rm --name "$client" --network "${project}_default" --network-alias product \
        --user "$(id -u):$(id -g)" "${socket[@]}" -v "$root:/repo:ro" -v "$tmp:/sandbox" \
        -e HOME=/sandbox/home -e UV_CACHE_DIR=/sandbox/uv-cache \
        -e UV_PROJECT_ENVIRONMENT=/sandbox/venv -e UV_PYTHON_DOWNLOADS=never \
        -e PYTHONDONTWRITEBYTECODE=1 -e PYTHONPATH=/repo/deploy/product -w /repo "$client_image" \
        uv run --locked --project sdk/python --quiet python "$@"
}

echo "== running the SDK integration example"
run_product deploy/sandbox/smoke.py --config /sandbox/sandbox.json

echo "== serving the reference product and driving one deposit through it"
run_product -m reference_product --config /sandbox/sandbox.json

echo "== running sandbox scenarios"
scenarios=(deploy/sandbox/scenarios/run.py --config /sandbox/sandbox.json)
restart=$(($# == 0))
others=()
for name in "$@"; do
    if [[ "$name" == restart_mid_flow ]]; then restart=1; else others+=("$name"); fi
done
status=0
if (($# == 0)); then
    run_product "${scenarios[@]}" --skip restart_mid_flow || status=1
elif ((${#others[@]})); then
    run_product "${scenarios[@]}" "${others[@]}" || status=1
fi
if ((restart)); then
    # Only this scenario gets the Docker socket, which it uses to restart the local service.
    run_product --docker-socket "${scenarios[@]}" restart_mid_flow || status=1
fi
exit "$status"
