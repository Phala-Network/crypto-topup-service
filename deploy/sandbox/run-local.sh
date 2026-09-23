#!/usr/bin/env bash
# Runs the Python integration example and the sandbox scenarios against a disposable local
# stack: deploy/local plus an Anvil chain (docker-compose.local.yml). Everything it starts is
# removed on exit. Usage: deploy/sandbox/run-local.sh [SCENARIO ...]
#
# Requires docker compose, Foundry (forge, cast), jq, and uv. Prices come from the
# live Coin Metrics, Binance, and Kraken endpoints, exactly as on Sepolia.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq uv; do
    require_command "$command"
done

project="topup-sandbox-$$"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-sandbox.XXXXXX")
compose=(docker compose -p "$project" -f "$root/deploy/local/docker-compose.yml"
    -f "$root/deploy/sandbox/docker-compose.local.yml")
# The product side (example and scenarios) runs in this image on the compose network, so the
# service reaches its endpoints as http://product:8089 even where a host firewall drops traffic
# from containers to the host.
client_image="ghcr.io/astral-sh/uv:0.11.14-python3.12-trixie-slim@sha256:13b5883729ec534af5863facf5c40ac0869f2c124ad5620c9ed36fe7c03fa87d"
client="$project-product"

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

free_port() {
    python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

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
export TOPUP_LOCAL_PORT=$(free_port)
export SANDBOX_ANVIL_PORT=$(free_port)
export TOPUP_LOCAL_ROUTES_DIR="$tmp/routes"
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
service_url="http://127.0.0.1:$TOPUP_LOCAL_PORT"
public_url="http://product:8089"
slug="sandbox-local"
keyid="$slug/v1"
mkdir -p "$TOPUP_LOCAL_ROUTES_DIR"

echo "== building and starting postgres, dstack simulator, and anvil"
"${compose[@]}" build postgres dstack-simulator topup
"${compose[@]}" up -d postgres dstack-simulator anvil
wait_for anvil cast chain-id --rpc-url "$rpc_url"
"${compose[@]}" run --rm migrate >"$tmp/migrate.log" 2>&1 || { cat "$tmp/migrate.log" >&2; exit 1; }

echo "== deploying the forwarder factory and sandbox test contracts"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
(cd "$root/contracts" && ADMIN="$owner" TREASURY="$owner" FOUNDRY_BROADCAST="$tmp/broadcast" \
    forge script script/DeployFactory.s.sol:DeployFactory --rpc-url "$rpc_url" \
    --private-key "$ANVIL_PRIVATE_KEY" --broadcast --silent)
factory=$(predicted_factory "$owner" "$owner")
implementation=$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")
"$root/deploy/sandbox/deploy-test-contracts.sh" --anvil-unlocked "$owner" \
    --rpc-url "$rpc_url" >"$tmp/contracts.json"
jq . "$tmp/contracts.json"

echo "== issuing product credentials and rendering the sandbox route"
uv run --locked --project "$root/sdk/python" topup-sdk keygen --keyid "$keyid" \
    --seed-out "$tmp/product.seed" >"$tmp/product-key.json"
FORWARDER_FACTORY="$factory" IMPLEMENTATION="$implementation" TREASURY="$owner" \
    TEST_TOKEN=$(jq -er .test_token "$tmp/contracts.json") \
    SANCTIONS_ORACLE=$(jq -er .sanctions_oracle "$tmp/contracts.json") \
    PRODUCT_SLUG="$slug" PRODUCT_KID="$keyid" SETTLEMENT_URL="$public_url/settlements" \
    RATE_LOCK_WINDOW_S=45 \
    "$root/deploy/sandbox/render-route.sh" >"$TOPUP_LOCAL_ROUTES_DIR/sandbox.yaml"
"${compose[@]}" run --rm --no-deps topup topup route validate /etc/topup/routes/sandbox.yaml
PSQL="${compose[*]} exec -T postgres psql -U postgres -d topup" \
    "$root/deploy/sandbox/issue-product.sh" --slug "$slug" --keyid "$keyid" \
    --public-key "$(jq -er .public_key "$tmp/product-key.json")" \
    --settlement-url "$public_url/settlements" --webhook-url "$public_url/webhooks" \
    --operator run-local --allow-http

echo "== starting the service"
"${compose[@]}" up -d topup
wait_for "GET /healthz" curl -fsS "$service_url/healthz"

# Addresses as seen from the product container on the compose network.
jq -n \
    --arg slug "$slug" --arg keyid "$keyid" --arg route "sandbox-$slug-tpha-usd" \
    --arg factory "$factory" --arg implementation "$implementation" \
    --arg token "$(jq -er .test_token "$tmp/contracts.json")" \
    --arg unsupported "$(jq -er .unsupported_token "$tmp/contracts.json")" \
    --arg public_url "$public_url" --arg payer "$owner" --arg topup "$project-topup-1" \
    '{service_url: "http://topup:8080", product_slug: $slug, product_keyid: $keyid,
      product_seed_file: "/sandbox/product.seed", route: $route, chain_id: 11155111,
      rpc_url: "http://anvil:8545", factory: $factory, implementation: $implementation,
      token: $token, token_symbol: "PHA", unsupported_token: $unsupported,
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
        -e PYTHONDONTWRITEBYTECODE=1 -w /repo "$client_image" \
        uv run --locked --project sdk/python --quiet python "$@"
}

echo "== running the Phala Cloud integration example"
run_product sdk/examples/phala_cloud_integration.py --config /sandbox/sandbox.json

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
