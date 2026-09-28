#!/usr/bin/env bash
# CVM rehearsal: runs the staging deployment artifact the way a CVM would, without Phala Cloud.
#
# 1. Builds the three images and pushes them to a throwaway loopback registry, so the composes are
#    rendered by deploy/render-compose.sh with immutable repository@sha256 references.
# 2. Starts Anvil with Sepolia's chain id, installs the canonical Multicall3 that Sepolia carries
#    (install_anvil_multicall3), and deploys the forwarder factory with the A2 scripts
#    (deploy/contracts: canonical proxy, mock Safe as the route treasury, deploy-factory.sh,
#    verify-deployment.sh), then the test token and sanctions oracle (deploy-test-contracts.sh).
# 3. Writes the staging route with those addresses, inlines it into the compose exactly where the
#    committed route lives, and renders the compose with the rehearsal's settings and the staging
#    domain, as Deploy provisions. dstack-ingress does not run (cvm-rehearsal.compose.yml).
# 4. Writes the unsealed `.env` with deploy/write-staging-env.sh, as Deploy does (exactly
#    the names of deploy/staging.env.example, all empty), and runs `docker compose up` on the
#    rendered file plus cvm-rehearsal.compose.yml (simulator, S3, Anvil), as dstack's app-compose
#    runner does. Without storage credentials PostgreSQL must refuse to initialize (the prefix
#    cannot be listed). Re-rendered with a changed setting (an upgrade), every service must be
#    recreated. Then it seals the secrets (the owner's `envs update`), and PostgreSQL initializes
#    from the provably empty prefix.
# 5. Asserts: migrate exits 0, topup passes its startup contract check and serves /healthz, the
#    attestation endpoint answers through the simulator and binds the flusher operator (matching
#    `topup attest --route`), the flusher operator is funded (the factory is permissionless),
#    Sentry reporting is off with the empty DSN, and WAL
#    archiving writes a fresh backup marker; the derived key and database credentials are mode 0600
#    files owned by PostgreSQL and in no container environment.
# 6. Runs the reference-product CVM the same way: deploy/product/docker-compose.yml rendered by
#    deploy/product/render-compose.sh with the pushed image and a provisional public URL, an
#    unsealed env from `write-staging-env.sh --product`, then the compose re-rendered with the real
#    public URL (the container must be recreated with the new config), then the sealed product
#    seed. One quote-first deposit, driven from another container with the deposit
#    driver (`python -m reference_product deposit`), is credited end to end and recorded
#    once in the product's ledger. Then it removes everything and asserts that no container,
#    volume, network, or image of the run is left.
#
# Nothing is bind-mounted and the workload publishes no host port (see cvm-rehearsal.compose.yml).
# Requires docker (Compose 2.24.4+), Foundry v1.8.3 with contracts/lib checked out, jq, python3,
# OpenSSL 3, and internet access for the live price sources, as in production.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq python3 openssl; do
    require_command "$command"
done

project="topup-cvm-rehearsal-$$"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-cvm-rehearsal.XXXXXX")
cvm="$tmp/cvm"
mkdir -p "$cvm"
: >"$cvm/.env"
registry_image="registry:3.1.1@sha256:325b4b29b041e82803abeb703e201655e4e23ab83264ec1a7c9ddb0a5b14a6e0"
# The pinned uv/Python image of deploy/sandbox/run-local.sh; it runs the SDK tools and the deposit
# driver (the reference product itself runs from its own image).
client_image="ghcr.io/astral-sh/uv:0.12.18-python3.14-trixie-slim@sha256:00facf17b58b02b725155862c5cd637f688f906bf7eb5b5194647886d8805cf3"
registry="$project-registry"
client="$project-client"
product_project="$project-product"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
export TOPUP_LOCAL_DSTACK_IMAGE="phala-pay-dstack-simulator:$project"
export SANDBOX_ANVIL_PORT
SANDBOX_ANVIL_PORT=$(free_port)
registry_port=$(free_port)
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
mapfile -t env_names < <(awk -F= '/^[[:space:]]*($|#)/ { next } { print $1 }' \
    "$root/deploy/staging.env.example")
local_images=()

# Compose as the CVM runs it: the rendered file with its `.env`. The staging names are removed
# from the calling environment so a developer's or CI's AWS_* or TOPUP_* cannot override
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

# The reference-product CVM: its rendered compose with its own `.env`, joined to the rehearsal
# network as `product` and publishing no host port (overlay written below).
pc() {
    docker compose --progress quiet -p "$product_project" --env-file "$tmp/product.env" \
        -f "$tmp/product.yml" -f "$tmp/product-overlay.yml" "$@"
}

leftovers() {
    {
        docker ps -aq --filter "label=com.docker.compose.project=$project"
        docker ps -aq --filter "label=com.docker.compose.project=$product_project"
        docker volume ls -q --filter "label=com.docker.compose.project=$product_project"
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
    if ((status != 0)) && [[ -f "$tmp/product.yml" ]]; then
        echo "--- product logs (last 40 lines) ---" >&2
        pc logs --no-color --tail 40 product >&2
    fi
    [[ -f "$tmp/product.yml" ]] && pc down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1
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

# Runs Python in the client container on the compose network.
product_python() {
    docker exec -i -e UV_PROJECT_ENVIRONMENT=/opt/venv -e UV_PYTHON_DOWNLOADS=never \
        -e PYTHONDONTWRITEBYTECODE=1 -e PYTHONPATH=/opt/repo/deploy/product -w /opt/repo "$client" \
        uv run --locked --project sdk/python --quiet python "$@"
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
publish phala-pay TOPUP_IMAGE "$root"
publish postgres-walg POSTGRES_WALG_IMAGE -f "$root/deploy/Dockerfile.postgres-walg" "$root"
publish phala-pay-reference-product PRODUCT_IMAGE \
    -f "$root/deploy/Dockerfile.reference-product" "$root"
docker build --quiet -t "$TOPUP_LOCAL_DSTACK_IMAGE" \
    -f "$root/deploy/local/Dockerfile.dstack-simulator" "$root" >/dev/null
echo "TOPUP_IMAGE=$TOPUP_IMAGE"
echo "POSTGRES_WALG_IMAGE=$POSTGRES_WALG_IMAGE"
echo "PRODUCT_IMAGE=$PRODUCT_IMAGE"

echo "== starting Anvil (chain id 11155111) and the client container"
dc up -d --wait anvil >/dev/null
# Sepolia carries the canonical Multicall3 that topup's balance and addressOf reads go through.
install_anvil_multicall3 "$rpc_url"
docker run -d --name "$client" --network "${project}_default" "$client_image" sleep infinity \
    >/dev/null
# `docker cp` streams through the API, so this works where the daemon cannot see the checkout.
docker exec "$client" mkdir /opt/repo
tar -C "$root" --exclude=.venv --exclude='*_cache' --exclude=__pycache__ -cf - sdk/python \
    deploy/product/reference_product |
    docker cp - "$client:/opt/repo"

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
    '{configured: true, networks: {sepolia: {chain_id: 11155111}}, treasury: $safe,
      safes: [{address: $safe, owners: [$owner], threshold: 1, proxy_code_hashes: [$code_hash],
               singleton: $singleton, singleton_code_hash: $singleton_code_hash,
               modules: [], guard: $zero, fallback_handler: $zero}]}' >"$tmp/safe-expectations.json"
"$DEPLOY_CONTRACTS_DIR/verify-safe.sh" --expectations "$tmp/safe-expectations.json" \
    --rpc "sepolia/a=$rpc_url" >/dev/null || die "verify-safe.sh rejected the mock treasury Safe"
PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
    "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" --rpc "sepolia/a=$rpc_url" --broadcast >/dev/null 2>&1 ||
    die "deploy-factory.sh failed"
"$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" \
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
# The committed staging route with real addresses; the reference product is its product.
sed -e "s|^\(  forwarder_factory: \).*|\1\"$factory\"|" \
    -e "s|^\(  treasury: \).*|\1\"$treasury\"|" \
    -e "s|^\(  contract: \).*|\1\"$token\"|" \
    -e "s|^\(  sanctions_oracle: \).*|\1\"$oracle\"|" \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml" >"$tmp/route.yaml"
if grep -Eiq '0x([0-9a-f])\1{39}' "$tmp/route.yaml"; then
    die "the rehearsal route still has a placeholder address"
fi
docker run --rm -i "$TOPUP_IMAGE" topup route validate /dev/stdin <"$tmp/route.yaml"
# The owner's admin key, in the PEM form deploy/runbooks/sign-admin-request.sh signs with.
openssl genpkey -algorithm ed25519 -out "$tmp/admin.pem"
admin_public_key=$(openssl pkey -in "$tmp/admin.pem" -pubout -outform DER | tail -c 32 | base64)
# Replace the inline route in a copy of the attested compose, as the operator's route commit
# does, then render it with the production renderer.
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
# render_topup ADMIN_KID: the settings Deploy renders from the `staging` Environment
# variables, for this network. Provider A is keyless, as staging's; provider B is attested with a
# `{key}` placeholder, as a paid provider is, and Anvil ignores the query that carries the key.
render_topup() {
    AWS_ENDPOINT=http://s3:3900 AWS_REGION=us-east-1 AWS_S3_FORCE_PATH_STYLE=true \
        WALG_S3_PREFIX=s3://topup-backups/postgres TOPUP_ADMIN_KID=$1 \
        TOPUP_ADMIN_PUBLIC_KEY=$admin_public_key SENTRY_ENVIRONMENT=staging \
        TOPUP_DOMAIN=pay-api-staging.phala.com \
        TOPUP_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network \
        TOPUP_RPC_PROVIDER_A_URL=http://anvil:8545 \
        TOPUP_RPC_PROVIDER_B_URL='http://anvil:8545/?key={key}' \
        "$root/deploy/render-compose.sh" "$tmp/docker-compose.yml" >"$cvm/docker-compose.yaml"
}
render_topup rehearsal-admin/v0
compose_file="$cvm/docker-compose.yaml"
dc config --format json >"$tmp/stack.json"
jq -j '.configs.topup_route_phala_cloud_sepolia_pha.content' "$tmp/stack.json" |
    cmp -s - "$tmp/route.yaml" || die "the rendered compose does not carry the rehearsal route"
jq -e --arg topup "$TOPUP_IMAGE" --arg postgres "$POSTGRES_WALG_IMAGE" \
    '[.services[] | select(.image | startswith("127.0.0.1:")) | .image] | unique == ([$topup, $postgres] | sort)' \
    "$tmp/stack.json" >/dev/null || die "the rendered compose does not use the pushed digests"
jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' "$tmp/stack.json" \
    >/dev/null || die "the rehearsal stack bind-mounts a host path"

echo "== writing the unsealed .env with write-staging-env.sh"
product_key=$(product_python -m topup_sdk keygen --keyid phala-cloud/v1 \
    --seed-out /opt/product.seed)
# The owner-sealed secrets, the only env values.
declare -A values=(
    [AWS_ACCESS_KEY_ID]=topup-s3
    [AWS_SECRET_ACCESS_KEY]=topup-s3-secret-key
    # Empty: the rehearsal proves the service runs unchanged with Sentry reporting off.
    [SENTRY_DSN]=''
    # Provider A's URL is keyless; topup reaches provider B only with its key in place of `{key}`.
    [TOPUP_RPC_PROVIDER_A_KEY]=''
    [TOPUP_RPC_PROVIDER_B_KEY]=rehearsal-rpc-key
)
((${#values[@]} == ${#env_names[@]})) || die "the rehearsal .env and staging.env.example differ"
for name in "${env_names[@]}"; do
    [[ -v "values[$name]" ]] || die "no rehearsal value for $name"
done
env -i PATH="$PATH" "$root/deploy/write-staging-env.sh" "$cvm/.env" >/dev/null
grep -qx 'AWS_SECRET_ACCESS_KEY=' "$cvm/.env" || die "the unsealed .env carries the S3 secret"
docker compose -f "$compose_file" config --variables | awk 'NR > 1 && NF > 0 { print $1 }' | sort >"$tmp/variables"
printf '%s\n' "${env_names[@]}" | sort | cmp -s - "$tmp/variables" ||
    die "the rendered compose reads other variables than staging.env.example"

echo "== docker compose up unsealed (the CVM's app-compose command)"
# Without credentials the backup prefix cannot be listed, so PostgreSQL refuses to initialize
# and never becomes healthy: `up` fails like dstack's boot, and nothing after it starts.
if dc up -d --remove-orphans >/dev/null 2>&1; then
    die "the unsealed stack started"
fi
refused() {
    dc logs --no-color postgres 2>&1 |
        grep -F 'the backup prefix could not be listed; refusing to initialize an empty data directory' \
            >/dev/null
}
wait_for "PostgreSQL to refuse initialization" 90 refused
if docker run --rm --entrypoint test -v "${project}_pgdata:/var/lib/postgresql" "$POSTGRES_WALG_IMAGE" \
    -e /var/lib/postgresql/data/PG_VERSION; then
    die "PostgreSQL initialized a cluster without listing the backup prefix"
fi
echo "ok: unsealed, PostgreSQL refuses to initialize without a listed backup prefix"

echo "== re-rendering with a changed setting (Deploy's upgrade)"
keys_before=$(dc ps -q keys)
render_topup rehearsal-admin/v1
dc up -d --remove-orphans >/dev/null 2>&1 || true
[[ -n "$(dc ps -q keys)" && "$(dc ps -q keys)" != "$keys_before" ]] ||
    die "a re-rendered setting did not recreate the services"
docker inspect --format '{{json .Config.Env}}' "$(dc ps -a -q topup)" |
    jq -e 'index("TOPUP_ADMIN_KID=rehearsal-admin/v1") != null' >/dev/null ||
    die "topup does not carry the re-rendered setting"
echo "ok: the re-rendered setting recreated every service"

echo "== sealing the secrets (the owner's envs update: same names, restart)"
for name in "${env_names[@]}"; do
    printf '%s=%s\n' "$name" "${values[$name]}"
done >"$cvm/.env"
dc up -d --remove-orphans >/dev/null
dc logs --no-color postgres 2>&1 |
    grep -F 'the backup prefix holds no base backup; initializing a new cluster' >/dev/null ||
    die "PostgreSQL did not initialize from the provably empty backup prefix"
echo "ok: sealed, PostgreSQL listed an empty backup prefix and initialized a new cluster"
# `keys` derives the backup key and the database credentials into files only PostgreSQL reads.
[[ "$(dc exec -T postgres stat -c '%a:%u:%g' /run/wal-g/backup.key /run/db-owner/postgres.password \
    /run/db-owner/postgres.pgpass /run/db-app/topup_service.pgpass | sort -u)" == 600:999:999 ]] ||
    die "the derived key and credential files are not postgres-owned mode 0600"
if dc ps -q | xargs docker inspect --format '{{range .Config.Env}}{{println .}}{{end}}' |
    grep -Eq '^(WALG_LIBSODIUM_KEY|POSTGRES_PASSWORD|PGPASSWORD)='; then
    die "a container environment carries a key or password"
fi
echo "ok: the derived key and credentials are mode 0600 files, in no container environment"
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
# The sealed SENTRY_DSN stays empty here: reporting must be off and the service unchanged.
dc logs --no-color topup 2>&1 | grep -F '"error reporting configured"' |
    grep -qF '"sentry_enabled":false' || die "topup did not start with Sentry reporting off"
echo "ok: topup runs with Sentry reporting off (empty SENTRY_DSN)"

# The owner learns the flusher operator only from /v1/attestation: production has no logs or SSH.
nonce=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
attestation=$(product_python - "$nonce" <<'PY'
import json, sys, httpx
from topup_client.models import AttestationResponse
from topup_sdk import verify_attestation_binding
nonce = bytes.fromhex(sys.argv[1])
body = httpx.get("http://topup:8080/v1/attestation", params={"nonce": nonce.hex()}, timeout=30).raise_for_status().json()
verify_attestation_binding(AttestationResponse.from_dict(body), nonce)
assert body["keyid"] == "settlement/v1", body["keyid"]
assert len(body["quote"]) > 0, "empty quote"
operators = body["operators"]
assert [(o["chain_id"], o["operator_key_version"], o["keyid"]) for o in operators] == [(11155111, 1, "operator/v1")], operators
print(json.dumps({key: body[key] for key in ("settlement_pubkey", "operators", "report_data")}))
PY
)
echo "ok: GET /v1/attestation binds the nonce, settlement key, and flusher operator (simulator quote)"
cli_attestation=$(dc exec -T topup topup attest --nonce "$nonce" \
    --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml |
    jq -c '{settlement_pubkey, operators, report_data}')
[[ "$(jq -S . <<<"$cli_attestation")" == "$(jq -S . <<<"$attestation")" ]] ||
    die "topup attest --route and GET /v1/attestation disagree"
echo "ok: topup attest --route reports the same operators and report_data"

operator=$(jq -er '.operators[0].address' <<<"$attestation")
# The factory is permissionless: the flusher needs no role, only gas.
cast send "$operator" --value 1ether --rpc-url "$rpc_url" --private-key "$ANVIL_PRIVATE_KEY" \
    >/dev/null
echo "ok: funded the attested flusher operator $operator"

marker_fresh() {
    local marker
    marker=$(dc exec -T backup cat /run/topup-observability/last-backup-unix-seconds) || return 1
    (($(date +%s) - marker <= 180))
}
wait_for "a fresh backup marker" 120 marker_fresh
echo "ok: WAL archiving refreshed the backup marker"

echo "== one quote-first deposit against the reference product"
# A CVM has no database access, so the owner issues the account through the signed admin API.
# `-j` omits the trailing newline, so the body passes through an argument byte for byte.
jq -cjn --arg public_key "$(jq -er .public_key <<<"$product_key")" \
    '{name: "phala-cloud", livemode: false, public_key: $public_key,
      webhook_url: "http://product:8089/webhooks"}' \
    >"$tmp/product.json"
mapfile -t headers < <("$root/deploy/runbooks/sign-admin-request.sh" POST \
    http://topup:8080/v1/admin/accounts "$tmp/product.json" "$tmp/admin.pem" rehearsal-admin/v1)
account=$(product_python - "$(<"$tmp/product.json")" "${headers[@]}" <<'PY'
import sys, httpx
headers = dict(header.split(": ", 1) for header in sys.argv[2:])
headers["content-type"] = "application/json"
response = httpx.post("http://topup:8080/v1/admin/accounts", content=sys.argv[1].encode(),
                      headers=headers, timeout=30)
assert response.status_code == 200, (response.status_code, response.text)
print(response.json()["id"])
PY
) || die "POST /v1/admin/accounts did not issue the account"
echo "ok: POST /v1/admin/accounts issued $account"

echo "== the reference-product CVM: rendered compose, unsealed env, public URL, then the sealed seed"
driver_key=$(product_python -m topup_sdk keygen --keyid driver/v1 --seed-out /opt/driver.seed)
# The committed product compose with this chain's addresses, as for the route above.
sed -e "s|^\(        \"factory\": \).*|\1\"$factory\",|" \
    -e "s|^\(        \"implementation\": \).*|\1\"$implementation\",|" \
    -e "s|^\(        \"treasury\": \).*|\1\"$treasury\",|" \
    -e "s|^\(        \"token\": \).*|\1\"$token\",|" \
    -e "s|^\(        \"product_slug\": \).*|\1\"$account\",|" \
    -e "s|^\(        \"product_keyid\": \).*|\1\"$account/v1\",|" \
    "$root/deploy/product/docker-compose.yml" >"$tmp/product-source.yml"
# render_product PUBLIC_URL: the settings Deploy (target `product`) renders, for this network.
render_product() {
    TOPUP_ORIGIN=http://topup:8080 PRODUCT_PUBLIC_URL=$1 PRODUCT_RPC_URL=http://anvil:8545 \
        PRODUCT_DRIVER_PUBLIC_KEY="$(jq -er .public_key <<<"$driver_key")" \
        "$root/deploy/product/render-compose.sh" "$tmp/product-source.yml" >"$tmp/product.yml"
}
render_product https://pending.invalid
grep -Fq "\"factory\": \"$factory\"," "$tmp/product.yml" || die "the product compose lacks the rehearsal factory"
cat >"$tmp/product-overlay.yml" <<YAML
services:
  product:
    ports: !reset []
    networks:
      default:
        aliases: [product]
networks:
  default:
    name: ${project}_default
    external: true
YAML
: >"$tmp/product.env"
env -i PATH="$PATH" "$root/deploy/write-staging-env.sh" --product "$tmp/product.env" >/dev/null
[[ "$(<"$tmp/product.env")" == PRODUCT_SEED= ]] || die "the unsealed product env is not only an empty seed"
pc up -d >/dev/null
product_healthy() {
    [[ "$(http_status http://product:8089/healthz)" == 200 ]]
}
wait_for "the product's /healthz" 90 product_healthy
echo "ok: the unsealed product pinned the settlement key and serves /healthz"
# Like the provisioning run's public-URL upgrade: a compose that differs only in the config content
# must recreate the container with the new config.
provisional=$(pc ps -q product)
render_product http://product:8089
pc up -d >/dev/null
[[ "$(pc ps -q product)" != "$provisional" ]] || die "a changed product setting did not recreate the container"
pc exec -T product cat /etc/product/config.json | jq -e '.public_url == "http://product:8089"' >/dev/null ||
    die "the recreated product does not read the re-rendered public URL"
wait_for "the product's /healthz after the public URL" 90 product_healthy
echo "ok: a re-rendered setting recreated the product with the new config"
seed=$(docker exec "$client" cat /opt/product.seed)
sed -i "s/^PRODUCT_SEED=\$/PRODUCT_SEED=$seed/" "$tmp/product.env"
unset seed
pc up -d >/dev/null
wait_for "the product's /healthz after sealing" 90 product_healthy
jq -n --arg factory "$factory" --arg implementation "$implementation" --arg token "$token" \
    --arg payer "$owner" --arg treasury "$treasury" --arg account "$account" \
    '{service_url: "http://topup:8080", product_slug: $account,
      product_keyid: ($account + "/v1"), route: "phala-cloud-sepolia-pha-usd", chain_id: 11155111,
      rpc_url: "http://anvil:8545", factory: $factory, implementation: $implementation,
      treasury: $treasury, token: $token, token_symbol: "PHA", public_url: "http://product:8089", payer: $payer}' |
    docker exec -i "$client" sh -c 'cat >/opt/driver.json'
product_python -m reference_product deposit --config /opt/driver.json \
    --driver-seed-file /opt/driver.seed --amount-minor 2500 --timeout 420
echo "ok: the deposit driver's quote-first deposit is credited once in the product's ledger"
# The route prices only from Coin Metrics, Binance, and Kraken over HTTPS, so a priced lock proves
# the distroless service image verified those servers with its system CA bundle.
priced_locks=$(dc exec -T postgres psql -U postgres -d topup -XAtq -c \
    "SELECT count(*) FROM quotes WHERE route = 'phala-cloud-sepolia-pha-usd' AND price_scaled > 0")
((priced_locks >= 1)) || die "no rate lock was priced from the live HTTPS sources"
echo "ok: topup priced a lock from live HTTPS price sources (TLS with system roots)"

echo "== workload memory (tdx.medium has 4 GiB)"
dc ps -q keys postgres topup heartbeat backup |
    xargs docker stats --no-stream --format '{{.Name}} {{.MemUsage}}' | tee "$tmp/memory"
echo "cvm-rehearsal: all checks passed"
