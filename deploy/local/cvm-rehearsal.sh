#!/usr/bin/env bash
# CVM rehearsal: runs the staging deployment artifact the way a CVM would, without Phala Cloud.
#
# 1. Builds both images and pushes them to a throwaway loopback registry, so the compose is
#    rendered by deploy/render-compose.sh with immutable repository@sha256 references.
# 2. Starts Anvil with Sepolia's chain id and deploys the forwarder factory with the A2 scripts
#    (deploy/contracts: canonical proxy, mock Safe as admin and treasury, deploy-factory.sh,
#    verify-deployment.sh), then the test token and sanctions oracle (deploy-test-contracts.sh).
# 3. Writes the staging route with those addresses, inlines it into the compose exactly where the
#    committed route lives, and renders the compose.
# 4. Writes a `.env` with exactly the names of deploy/staging.env.example and runs
#    `docker compose up` on the rendered file plus cvm-rehearsal.compose.yml (simulator, MinIO,
#    Anvil), as dstack's app-compose runner does.
# 5. Asserts: migrate exits 0, topup passes its startup contract check and serves /healthz, the
#    attestation endpoint answers through the simulator, the backup marker is fresh, and one
#    quote-first deposit is credited end to end against the reference product
#    (sdk/examples/phala_cloud_integration.py). Then it removes everything and asserts that no
#    container, volume, network, or image of the run is left.
#
# Nothing is bind-mounted and the workload publishes no host port (see cvm-rehearsal.compose.yml).
# Requires docker (Compose 2.24.4+), Foundry v1.8.3 with contracts/lib checked out, jq, python3,
# and internet access for the live price sources, as in production.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq python3; do
    require_command "$command"
done

project="topup-cvm-rehearsal-$$"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-cvm-rehearsal.XXXXXX")
cvm="$tmp/cvm"
mkdir -p "$cvm"
: >"$cvm/.env"
registry_image="registry:3.1.1@sha256:325b4b29b041e82803abeb703e201655e4e23ab83264ec1a7c9ddb0a5b14a6e0"
# The pinned uv/Python image of deploy/sandbox/run-local.sh; it plays the reference product.
client_image="ghcr.io/astral-sh/uv:0.12.18-python3.14-trixie-slim@sha256:00facf17b58b02b725155862c5cd637f688f906bf7eb5b5194647886d8805cf3"
registry="$project-registry"
client="$project-product"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
export TOPUP_LOCAL_DSTACK_IMAGE="crypto-topup-dstack-simulator:$project"
export SANDBOX_ANVIL_PORT
SANDBOX_ANVIL_PORT=$(free_port)
registry_port=$(free_port)
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
mapfile -t env_names < <(awk -F= '/^[[:space:]]*($|#)/ { next } { print $1 }' \
    "$root/deploy/staging.env.example")
local_images=()

# Compose as the CVM runs it: the rendered file with its `.env`. The staging names are removed
# from the calling environment so a developer's or CI's DATABASE_URL or AWS_* cannot override
# the `.env` file. The project directory anchors the overlay's `extends` paths.
dc() {
    local unset=() name
    for name in "${env_names[@]}"; do
        unset+=(-u "$name")
    done
    env "${unset[@]}" docker compose --progress quiet -p "$project" --project-directory "$root/deploy/local" \
        --env-file "$cvm/.env" -f "${compose_file:-$root/deploy/docker-compose.yml}" \
        -f "$root/deploy/local/cvm-rehearsal.compose.yml" "$@"
}

leftovers() {
    {
        docker ps -aq --filter "label=com.docker.compose.project=$project"
        docker ps -aq --filter "name=^$registry\$" --filter "name=^$client\$"
        docker volume ls -q --filter "label=com.docker.compose.project=$project"
        docker network ls -q --filter "label=com.docker.compose.project=$project"
        local image
        for image in "${local_images[@]}" "$TOPUP_LOCAL_DSTACK_IMAGE"; do
            docker image inspect --format "image $image" "$image"
        done
    } 2>/dev/null
}

cleanup() {
    status=$?
    set +e
    if ((status != 0)); then
        echo "--- topup logs (last 60 lines) ---" >&2
        dc logs --no-color --tail 60 topup >&2
    fi
    docker rm -f "$client" >/dev/null 2>&1
    dc down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1
    docker rm -f -v "$registry" >/dev/null 2>&1
    # Failures show up in leftovers() below.
    docker image rm "${local_images[@]}" "$TOPUP_LOCAL_DSTACK_IMAGE" >/dev/null 2>&1
    rm -rf "$tmp"
    if [[ -n "$(leftovers)" ]]; then
        echo "FAIL: containers, volumes, networks, or images of $project were left behind:" >&2
        leftovers >&2
        status=1
    else
        echo "== shutdown left no container, volume, network, or image of $project"
    fi
    exit "$status"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

wait_for() {
    local description=$1 attempts=$2
    shift 2
    until "$@" >/dev/null 2>&1; do
        attempts=$((attempts - 1))
        ((attempts > 0)) || { echo "timed out waiting for $description" >&2; return 1; }
        sleep 2
    done
}

# Runs Python in the reference-product container on the compose network.
product_python() {
    docker exec -i -e UV_PROJECT_ENVIRONMENT=/opt/venv -e UV_PYTHON_DOWNLOADS=never \
        -e PYTHONDONTWRITEBYTECODE=1 -w /opt/sdk "$client" \
        uv run --locked --project python --quiet python "$@"
}

echo "== building images and pushing them to a loopback registry"
docker run -d --name "$registry" -p "127.0.0.1:$registry_port:5000" \
    --mount type=tmpfs,destination=/var/lib/registry "$registry_image" >/dev/null
wait_for "the registry" 30 curl -fsS "http://127.0.0.1:$registry_port/v2/"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
# publish NAME VARIABLE BUILD_ARGS...: builds, pushes, and sets VARIABLE to repository@sha256.
publish() {
    local tag="127.0.0.1:$registry_port/$1:rehearsal" variable=$2 digest
    shift 2
    docker build --quiet --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" -t "$tag" "$@" \
        >/dev/null
    local_images+=("$tag")
    docker push --quiet "$tag" >/dev/null
    digest=$(docker image inspect --format '{{json .RepoDigests}}' "$tag" |
        jq -er --arg repository "${tag%:rehearsal}" '.[] | select(startswith($repository + "@"))')
    local_images+=("$digest")
    printf -v "$variable" '%s' "$digest"
    export "${variable?}"
}
publish crypto-topup TOPUP_IMAGE "$root"
publish postgres-walg POSTGRES_WALG_IMAGE -f "$root/deploy/Dockerfile.postgres-walg" "$root"
docker build --quiet -t "$TOPUP_LOCAL_DSTACK_IMAGE" \
    -f "$root/deploy/local/Dockerfile.dstack-simulator" "$root" >/dev/null
echo "TOPUP_IMAGE=$TOPUP_IMAGE"
echo "POSTGRES_WALG_IMAGE=$POSTGRES_WALG_IMAGE"

echo "== starting Anvil (chain id 11155111) and the reference-product container"
dc up -d --wait anvil >/dev/null
docker run -d --name "$client" --network "${project}_default" --network-alias product \
    "$client_image" sleep infinity >/dev/null
# `docker cp` streams through the API, so this works where the daemon cannot see the checkout.
docker exec "$client" mkdir /opt/sdk
tar -C "$root/sdk" --exclude=.venv --exclude='*_cache' --exclude=__pycache__ -cf - python examples |
    docker cp - "$client:/opt/sdk"

echo "== deploying contracts with the A2 and sandbox scripts"
export FOUNDRY_BROADCAST="$tmp/broadcast"
"$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$rpc_url" --local-fund --broadcast >/dev/null
nonce=$(cast nonce "$owner" --rpc-url "$rpc_url")
safe_singleton=$(cast compute-address "$owner" --nonce "$nonce" | awk '{print $NF}')
treasury=$(cast compute-address "$owner" --nonce $((nonce + 1)) | awk '{print $NF}')
(cd "$CONTRACTS_DIR" && SAFE_OWNER="$owner" forge script test/DeployMockSafe.s.sol:DeployMockSafe \
    --rpc-url "$rpc_url" --broadcast --private-key "$ANVIL_PRIVATE_KEY" -q) >/dev/null
[[ "$(cast code "$treasury" --rpc-url "$rpc_url")" != 0x ]] || die "mock Safe was not deployed"
jq -n --arg safe "$treasury" --arg owner "$owner" \
    --arg code_hash "$(code_hash "$rpc_url" "$treasury")" --arg singleton "$safe_singleton" \
    --arg singleton_code_hash "$(code_hash "$rpc_url" "$safe_singleton")" --arg zero "$ZERO_ADDRESS" \
    '{configured: true, networks: {sepolia: {chain_id: 11155111}}, admin: $safe, treasury: $safe,
      safes: [{address: $safe, owners: [$owner], threshold: 1, proxy_code_hashes: [$code_hash],
               singleton: $singleton, singleton_code_hash: $singleton_code_hash,
               modules: [], guard: $zero, fallback_handler: $zero}]}' >"$tmp/safe-expectations.json"
ADMIN="$treasury" TREASURY="$treasury" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
    "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" --rpc "sepolia/a=$rpc_url" --broadcast \
    --safe-expectations "$tmp/safe-expectations.json" >/dev/null 2>&1 ||
    die "deploy-factory.sh failed"
ADMIN="$treasury" TREASURY="$treasury" "$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" \
    --safe-expectations "$tmp/safe-expectations.json" \
    --rpc "sepolia/a=$rpc_url" --rpc "sepolia/b=$rpc_url" >"$tmp/verification.json" ||
    die "verify-deployment.sh failed: $(jq -c '[.chains[].checks]' "$tmp/verification.json")"
factory=$(jq -er '.chains[0].factory' "$tmp/verification.json")
implementation=$(jq -er '.chains[0].implementation' "$tmp/verification.json")
"$root/deploy/sandbox/deploy-test-contracts.sh" --anvil-unlocked "$owner" --rpc-url "$rpc_url" \
    >"$tmp/test-contracts.json"
token=$(jq -er .test_token "$tmp/test-contracts.json")
oracle=$(jq -er .sanctions_oracle "$tmp/test-contracts.json")
printf 'factory=%s implementation=%s treasury=%s token=%s sanctions_oracle=%s\n' \
    "$factory" "$implementation" "$treasury" "$token" "$oracle"

echo "== writing the route and rendering the staging compose"
# The committed staging route with real addresses; the reference product stands in for the
# product's settlement endpoint.
sed -e "s|^\(    forwarder_factory: \).*|\1\"$factory\"|" \
    -e "s|^\(    implementation: \).*|\1\"$implementation\"|" \
    -e "s|^\(    treasury: \).*|\1\"$treasury\"|" \
    -e "s|^\(  contract: \).*|\1\"$token\"|" \
    -e "s|^\(  sanctions_oracle: \).*|\1\"$oracle\"|" \
    -e "s|^\(  settlement_url: \).*|\1\"http://product:8089/settlements\"|" \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" >"$tmp/route.yaml"
if grep -Eiq '0x([0-9a-f])\1{39}' "$tmp/route.yaml"; then
    die "the rehearsal route still has a placeholder address"
fi
docker run --rm -i "$TOPUP_IMAGE" topup route validate /dev/stdin <"$tmp/route.yaml"
# Replace the inline route in a copy of the attested compose, as the operator's route commit
# does, then render image digests with the production renderer.
awk -v route="$tmp/route.yaml" '
    /^  topup_route_phala_cloud_sepolia_pha:$/ { print; in_config = 1; next }
    in_config && /^    content: [|]$/ {
        print
        while ((getline line < route) > 0) print (line == "" ? "" : "      " line)
        skipping = 1
        next
    }
    skipping && (/^      / || /^$/) { next }
    { skipping = 0; in_config = 0; print }
' "$root/deploy/docker-compose.yml" >"$tmp/docker-compose.yml"
"$root/deploy/render-compose.sh" "$tmp/docker-compose.yml" >"$cvm/docker-compose.yaml"
compose_file="$cvm/docker-compose.yaml"
dc config --format json >"$tmp/stack.json"
jq -j '.configs.topup_route_phala_cloud_sepolia_pha.content' "$tmp/stack.json" |
    cmp -s - "$tmp/route.yaml" || die "the rendered compose does not carry the rehearsal route"
jq -e --arg topup "$TOPUP_IMAGE" --arg postgres "$POSTGRES_WALG_IMAGE" \
    '[.services[] | select(.image | startswith("127.0.0.1:")) | .image] | unique == ([$topup, $postgres] | sort)' \
    "$tmp/stack.json" >/dev/null || die "the rendered compose does not use the pushed digests"
jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' "$tmp/stack.json" \
    >/dev/null || die "the rehearsal stack bind-mounts a host path"

echo "== writing .env with exactly the staging.env.example names"
admin_key=$(product_python -m topup_sdk keygen --keyid rehearsal-admin/v1 \
    --seed-out /tmp/admin.seed)
product_key=$(product_python -m topup_sdk keygen --keyid phala-cloud/v1 \
    --seed-out /opt/product.seed)
owner_password=$(openssl rand -hex 32)
app_password=$(openssl rand -hex 32)
declare -A values=(
    [AWS_ACCESS_KEY_ID]=topup-minio
    [AWS_ENDPOINT]=http://minio:9000
    [AWS_REGION]=us-east-1
    [AWS_S3_FORCE_PATH_STYLE]=true
    [AWS_SECRET_ACCESS_KEY]=topup-minio-secret
    [AWS_SESSION_TOKEN]=
    [COINMETRICS_API_KEY]=
    [DATABASE_URL]="postgres://topup_service:$app_password@postgres:5432/topup"
    [MIGRATE_DATABASE_URL]="postgres://postgres:$owner_password@postgres:5432/topup"
    [POSTGRES_PASSWORD]="$owner_password"
    [TOPUP_ADMIN_KID]=rehearsal-admin/v1
    [TOPUP_ADMIN_PUBLIC_KEY]=$(jq -er .public_key <<<"$admin_key")
    [TOPUP_BACKUP_KEY_FALLBACK_VERSIONS]=0
    [TOPUP_BACKUP_KEY_VERSION]=1
    [TOPUP_APP_PASSWORD]="$app_password"
    [TOPUP_PUBLIC_ORIGIN]=http://topup:8080
    [TOPUP_RPC_PROVIDER_A_URL]=http://anvil:8545
    [TOPUP_RPC_PROVIDER_B_URL]=http://anvil:8545
    [TOPUP_WAL_ARCHIVE]=on
    [WALG_S3_PREFIX]=s3://topup-backups/postgres
)
((${#values[@]} == ${#env_names[@]})) || die "the rehearsal .env and staging.env.example differ"
for name in "${env_names[@]}"; do
    [[ -v "values[$name]" ]] || die "no rehearsal value for $name"
    printf '%s=%s\n' "$name" "${values[$name]}"
done >"$cvm/.env"
docker compose -f "$compose_file" config --variables | awk 'NR > 1 && NF > 0 { print $1 }' | sort >"$tmp/variables"
printf '%s\n' "${env_names[@]}" | sort | cmp -s - "$tmp/variables" ||
    die "the rendered compose reads other variables than staging.env.example"

echo "== docker compose up (the CVM's app-compose command)"
dc up -d --remove-orphans >/dev/null
migrate_exited() {
    [[ "$(dc ps -a --format json migrate | jq -rs 'flatten | .[0].State')" == exited ]]
}
wait_for "migrate" 90 migrate_exited
migrate_exit=$(dc ps -a --format json migrate | jq -rs 'flatten | .[0].ExitCode')
[[ "$migrate_exit" == 0 ]] || { dc logs migrate >&2; die "migrate exited with $migrate_exit"; }
echo "ok: migrate exited 0"

http_status() {
    product_python -c 'import sys, httpx; print(httpx.get(sys.argv[1], timeout=5).status_code)' "$1"
}
healthy() {
    [[ "$(http_status http://topup:8080/healthz)" == 200 ]]
}
wait_for "GET /healthz" 90 healthy
# `topup run` checks the route's contracts on every provider before it touches the database
# and binds the listener, so a served /healthz means the check passed.
if dc logs topup 2>&1 | grep -q 'on-chain contract check failed'; then
    die "topup logged a failed startup contract check"
fi
echo "ok: topup passed its startup contract check; GET /healthz is 200"

product_python - <<'PY'
import hashlib, secrets, httpx
nonce = secrets.token_bytes(32)
body = httpx.get("http://topup:8080/v1/attestation", params={"nonce": nonce.hex()}, timeout=30).raise_for_status().json()
key = bytes.fromhex(body["settlement_pubkey"])
assert body["keyid"] == "settlement/v1", body["keyid"]
assert bytes.fromhex(body["report_data"]) == hashlib.sha256(nonce + key).digest(), "report_data"
assert len(body["quote"]) > 0, "empty quote"
print(f"ok: GET /v1/attestation answered through the simulator ({len(body['quote']) // 2} quote bytes)")
PY

marker_fresh() {
    local marker
    marker=$(dc exec -T backup cat /run/topup-observability/last-backup-unix-seconds) || return 1
    (($(date +%s) - marker <= 180))
}
wait_for "a fresh backup marker" 120 marker_fresh
# The service reads the marker on its metrics refresh; a missing marker exports zero.
marker_exported() {
    product_python -c '
import httpx, sys
text = httpx.get("http://topup:9464/metrics", timeout=5).raise_for_status().text
values = [float(line.split()[-1]) for line in text.splitlines()
          if line.startswith("topup_backup_last_success_unixtime_seconds{")]
sys.exit(0 if values and min(values) > 0 else 1)'
}
wait_for "topup to export the backup marker" 60 marker_exported
echo "ok: WAL archiving refreshed the backup marker and topup exports it"

echo "== one quote-first deposit against the reference product"
PSQL="docker compose -p $project --project-directory $root/deploy/local --env-file $cvm/.env -f $compose_file -f $root/deploy/local/cvm-rehearsal.compose.yml exec -T postgres psql -U postgres -d topup" \
    "$root/deploy/sandbox/issue-product.sh" --slug phala-cloud \
    --public-key "$(jq -er .public_key <<<"$product_key")" \
    --webhook-url http://product:8089/webhooks --operator cvm-rehearsal --allow-http >/dev/null
jq -n --arg factory "$factory" --arg implementation "$implementation" --arg token "$token" \
    --arg payer "$owner" \
    '{service_url: "http://topup:8080", product_slug: "phala-cloud",
      product_keyid: "phala-cloud/v1", product_seed_file: "/opt/product.seed",
      route: "phala-cloud-sepolia-pha-usd", chain_id: 11155111, rpc_url: "http://anvil:8545",
      factory: $factory, implementation: $implementation, token: $token, token_symbol: "PHA",
      listen_host: "0.0.0.0", listen_port: 8089, public_url: "http://product:8089",
      payer: $payer}' | docker exec -i "$client" sh -c 'cat >/opt/rehearsal.json'
product_python examples/phala_cloud_integration.py --config /opt/rehearsal.json

echo "== workload memory (tdx.medium has 4 GiB)"
docker stats --no-stream --format '{{.Name}} {{.MemUsage}}' \
    $(dc ps -q backup-key postgres topup heartbeat backup) | tee "$tmp/memory"
echo "cvm-rehearsal: all checks passed"
