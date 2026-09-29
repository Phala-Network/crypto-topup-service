#!/usr/bin/env bash
# CVM rehearsal: runs the staging deployment artifact the way a CVM would, without Phala Cloud.
#
# 1. Builds the three images and pushes them to a throwaway loopback registry, so the composes are
#    rendered by deploy/render-compose.sh with immutable repository@sha256 references.
# 2. Starts Anvil with Sepolia's chain id, installs the canonical Multicall3 that Sepolia carries
#    (install_anvil_multicall3), and deploys the forwarder factory with the A2 scripts
#    (deploy/contracts: canonical proxy, mock Safe checked by verify-safe.sh, deploy-factory.sh,
#    verify-deployment.sh), then the test token and sanctions oracle (deploy-test-contracts.sh).
#    A second Anvil with Base Sepolia's chain id gets the same, without the Safe, for the Base
#    Sepolia routes.
# 3. Writes the staging routes with those addresses, inlines them into the compose exactly where the
#    committed routes live, and renders the compose with the rehearsal's settings and the staging
#    domain, as Deploy provisions. dstack-ingress does not run (cvm-rehearsal.compose.yml).
# 4. Writes the unsealed `.env` with deploy/write-staging-env.sh, as Deploy does (exactly
#    the names of deploy/staging.env.example, all empty), and runs `docker compose up` on the
#    rendered file plus cvm-rehearsal.compose.yml (simulator, S3, Anvil), as dstack's app-compose
#    runner does. Without storage credentials PostgreSQL must refuse to initialize (the prefix
#    cannot be listed). Re-rendered with a changed setting (an upgrade), every service must be
#    recreated. Then it seals the secrets (the owner's `envs update`), and PostgreSQL initializes
#    from the provably empty prefix.
# 5. Asserts: migrate exits 0, topup passes its startup contract check and serves /healthz, the
#    attestation endpoint answers an account's API key through the simulator and binds the account's
#    webhook key (matching `topup attest`), Sentry reporting is off with the empty DSN, and WAL
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
export SANDBOX_ANVIL_PORT REHEARSAL_BASE_SEPOLIA_ANVIL_PORT
SANDBOX_ANVIL_PORT=$(free_port)
REHEARSAL_BASE_SEPOLIA_ANVIL_PORT=$(free_port)
registry_port=$(free_port)
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
base_rpc_url="http://127.0.0.1:$REHEARSAL_BASE_SEPOLIA_ANVIL_PORT"
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

echo "== starting Anvil (chain ids 11155111 and 84532) and the client container"
dc up -d --wait anvil anvil-base-sepolia >/dev/null
# Both chains carry the canonical Multicall3 that topup's balance and addressOf reads go through.
install_anvil_multicall3 "$rpc_url"
install_anvil_multicall3 "$base_rpc_url"
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
safe=$(cast compute-address "$owner" --nonce $((nonce + 1)) | awk '{print $NF}')
(cd "$CONTRACTS_DIR" && SAFE_OWNER="$owner" forge script test/DeployMockSafe.s.sol:DeployMockSafe \
    --rpc-url "$rpc_url" --broadcast --private-key "$ANVIL_PRIVATE_KEY" -q) >/dev/null
[[ "$(cast code "$safe" --rpc-url "$rpc_url")" != 0x ]] || die "mock Safe was not deployed"
jq -n --arg safe "$safe" --arg owner "$owner" \
    --arg code_hash "$(code_hash "$rpc_url" "$safe")" --arg singleton "$safe_singleton" \
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
printf 'factory=%s implementation=%s safe=%s token=%s sanctions_oracle=%s\n' \
    "$factory" "$implementation" "$safe" "$token" "$oracle"
# Base Sepolia: the same deterministic factory, and test contracts of its own.
"$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$base_rpc_url" --local-fund --broadcast >/dev/null
PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
    "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" --rpc "base-sepolia/a=$base_rpc_url" --broadcast \
    >/dev/null 2>&1 || die "deploy-factory.sh failed on Base Sepolia"
"$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" \
    --rpc "base-sepolia/a=$base_rpc_url" --rpc "base-sepolia/b=$base_rpc_url" \
    >"$tmp/base-verification.json" ||
    die "verify-deployment.sh failed on Base Sepolia: $(jq -c '[.chains[].checks]' "$tmp/base-verification.json")"
[[ "$(jq -er '.chains[0].factory' "$tmp/base-verification.json")" == "$factory" ]] ||
    die "the Base Sepolia factory is not the Sepolia one"
"$root/deploy/sandbox/deploy-test-contracts.sh" --anvil-unlocked "$owner" --rpc-url "$base_rpc_url" \
    >"$tmp/base-test-contracts.json"
base_token=$(jq -er .test_token "$tmp/base-test-contracts.json")
base_second_token=$(jq -er .unsupported_token "$tmp/base-test-contracts.json")
base_oracle=$(jq -er .sanctions_oracle "$tmp/base-test-contracts.json")
printf 'base-sepolia: token=%s second_token=%s sanctions_oracle=%s\n' \
    "$base_token" "$base_second_token" "$base_oracle"

echo "== writing the routes and rendering the staging compose"
# The committed staging routes with their chain's addresses, one file per inline config. On each
# chain the test token stands in for PHA, the reference product's asset, and the second mock token
# for USDC.
second_token=$(jq -er .unsupported_token "$tmp/test-contracts.json")
mkdir "$tmp/routes"
for path in "$root"/deploy/config/routes/*.yaml; do
    name=$(basename "$path" .yaml)
    case "$name" in
        phala-cloud-sepolia-pha) asset=$token route_oracle=$oracle ;;
        phala-cloud-sepolia-usdc) asset=$second_token route_oracle=$oracle ;;
        phala-cloud-base-sepolia-pha) asset=$base_token route_oracle=$base_oracle ;;
        phala-cloud-base-sepolia-usdc) asset=$base_second_token route_oracle=$base_oracle ;;
        *) die "no rehearsal token for the route $name" ;;
    esac
    route="$tmp/routes/topup_route_${name//-/_}.yaml"
    sed -e "s|^\(  forwarder_factory: \).*|\1\"$factory\"|" \
        -e "s|^\(  contract: \).*|\1\"$asset\"|" \
        -e "s|^\(  sanctions_oracle: \).*|\1\"$route_oracle\"|" \
        "$path" >"$route"
    if grep -Eiq '0x([0-9a-f])\1{39}' "$route"; then
        die "the rehearsal route $name still has a placeholder address"
    fi
    docker run --rm -i "$TOPUP_IMAGE" topup route validate /dev/stdin <"$route"
done
# The owner's admin key, in the PEM form deploy/runbooks/sign-admin-request.sh signs with.
openssl genpkey -algorithm ed25519 -out "$tmp/admin.pem"
admin_public_key=$(openssl pkey -in "$tmp/admin.pem" -pubout -outform DER | tail -c 32 | base64)
# Replace the inline routes in a copy of the attested compose, as the operator's route commit
# does, then render it with the production renderer.
awk -v routes="$tmp/routes" '
    /^  topup_route_[a-z0-9_]+:$/ {
        print
        route = routes "/" substr($1, 1, length($1) - 1) ".yaml"
        skipping = 0
        next
    }
    route != "" && /^    content: [|]$/ {
        print
        while ((getline line < route) > 0) print (line == "" ? "" : "      " line)
        close(route)
        skipping = 1
        next
    }
    skipping && (/^      / || /^$/) { next }
    { skipping = 0; route = ""; print }
' "$root/deploy/docker-compose.yml" >"$tmp/docker-compose.yml"
# render_topup ADMIN_KID: the settings Deploy renders from the `staging` Environment
# variables, for this network. Provider A is keyless, as staging's; provider B is attested with a
# `{key}` placeholder, as a paid provider is, and Anvil ignores the query that carries the key.
# Base Sepolia's two providers are keyless, as staging's, at two URLs of its Anvil.
render_topup() {
    AWS_ENDPOINT=http://s3:3900 AWS_REGION=us-east-1 AWS_S3_FORCE_PATH_STYLE=true \
        WALG_S3_PREFIX=s3://topup-backups/postgres TOPUP_ADMIN_KID=$1 \
        TOPUP_ADMIN_PUBLIC_KEY=$admin_public_key SENTRY_ENVIRONMENT=staging \
        TOPUP_DOMAIN=pay-api-staging.phala.com \
        TOPUP_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network \
        TOPUP_RPC_PROVIDER_A_URL=http://anvil:8545 \
        TOPUP_RPC_PROVIDER_B_URL='http://anvil:8545/?key={key}' \
        TOPUP_RPC_BASE_SEPOLIA_A_URL=http://anvil-base-sepolia:8545 \
        TOPUP_RPC_BASE_SEPOLIA_B_URL='http://anvil-base-sepolia:8545/?provider=b' \
        "$root/deploy/render-compose.sh" "$tmp/docker-compose.yml" >"$cvm/docker-compose.yaml"
}
render_topup rehearsal-admin/v0
compose_file="$cvm/docker-compose.yaml"
dc config --format json >"$tmp/stack.json"
jq -r '.configs | keys[] | select(startswith("topup_route_"))' "$tmp/stack.json" |
    LC_ALL=C sort >"$tmp/configs"
for route in "$tmp"/routes/*.yaml; do
    basename "$route" .yaml
done | LC_ALL=C sort | cmp -s - "$tmp/configs" ||
    die "the rendered compose does not carry one inline config per rehearsal route"
for route in "$tmp"/routes/*.yaml; do
    jq -j --arg name "$(basename "$route" .yaml)" '.configs[$name].content' "$tmp/stack.json" |
        cmp -s - "$route" || die "the rendered compose does not carry the rehearsal route $route"
done
jq -e --arg topup "$TOPUP_IMAGE" --arg postgres "$POSTGRES_WALG_IMAGE" \
    '[.services[] | select(.image | startswith("127.0.0.1:")) | .image] | unique == ([$topup, $postgres] | sort)' \
    "$tmp/stack.json" >/dev/null || die "the rendered compose does not use the pushed digests"
jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' "$tmp/stack.json" \
    >/dev/null || die "the rehearsal stack bind-mounts a host path"

echo "== writing the unsealed .env with write-staging-env.sh"
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

marker_fresh() {
    local marker
    marker=$(dc exec -T backup cat /run/topup-observability/last-backup-unix-seconds) || return 1
    (($(date +%s) - marker <= 180))
}
wait_for "a fresh backup marker" 120 marker_fresh
echo "ok: WAL archiving refreshed the backup marker"

echo "== one quote-first deposit against the reference product"
# A CVM has no database access, so the operator creates the account through the signed admin API;
# the answer's first test key goes straight to the client container, never to this shell's output.
# `-j` omits the trailing newline, so the body passes through an argument byte for byte.
jq -cjn '{name: "phala-cloud", contact: {name: "Rehearsal", email: "rehearsal@example.com"},
      due_diligence: {reference: "rehearsal", reviewed_at: "2026-09-28", reviewed_by: "cvm-rehearsal"},
      charges_enabled: false, reason: "CVM rehearsal"}' \
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
account = response.json()
secret = account["api_keys"][0]["secret"]
with open("/opt/product.key", "w", encoding="ascii") as key:
    key.write(secret)
# The merchant, not the operator, registers its webhook endpoint, with its own key.
endpoint = httpx.post("http://topup:8080/v1/webhook_endpoints",
                      json={"url": "http://product:8089/webhooks", "enabled_events": ["*"]},
                      headers={"Authorization": f"Bearer {secret}"}, timeout=30)
assert endpoint.status_code == 200, (endpoint.status_code, endpoint.text)
print(account["id"])
PY
) || die "POST /v1/admin/accounts did not create the account and its webhook endpoint"
echo "ok: POST /v1/admin/accounts created $account with its first test key; its endpoint is registered"

# The account proves its test-mode treasury through the API (design D10), here the owner EOA: the
# mock Safe above implements no EIP-1271. The challenge is signed on this host with `cast`.
treasury=$owner
message=$(product_python - "$treasury" <<'PY'
import sys, httpx
with open("/opt/product.key", encoding="ascii") as key:
    headers = {"Authorization": f"Bearer {key.read().strip()}"}
response = httpx.post("http://topup:8080/v1/treasuries/challenge", headers=headers, timeout=30,
                      json={"chain_id": 11155111, "address": sys.argv[1]})
assert response.status_code == 200, (response.status_code, response.text)
print(response.json()["message"], end="")
PY
) || die "POST /v1/treasuries/challenge failed"
signature=$(cast wallet sign --private-key "$ANVIL_PRIVATE_KEY" "$message")
product_python - "$message" "$signature" <<'PY' || die "POST /v1/treasuries did not set the treasury"
import sys, httpx
with open("/opt/product.key", encoding="ascii") as key:
    headers = {"Authorization": f"Bearer {key.read().strip()}"}
response = httpx.post("http://topup:8080/v1/treasuries", headers=headers, timeout=30,
                      json={"chain_id": 11155111, "message": sys.argv[1], "signature": sys.argv[2]})
assert response.status_code == 200, (response.status_code, response.text)
assert response.json()["status"] == "active", response.text
PY
echo "ok: POST /v1/treasuries set the account's treasury $treasury with a signed challenge"

# The merchant learns its webhook key only from /v1/attestation, with its API key: production has
# no logs or SSH.
nonce=$(python3 -c 'import secrets; print(secrets.token_hex(32))')
attestation=$(product_python - "$nonce" "$account" <<'PY'
import json, sys, httpx
from topup_client.models import AttestationResponse
from topup_sdk import verify_attestation_binding
nonce, account = bytes.fromhex(sys.argv[1]), sys.argv[2]
url = "http://topup:8080/v1/attestation"
anonymous = httpx.get(url, params={"nonce": nonce.hex()}, timeout=30)
assert anonymous.status_code == 401, anonymous.status_code
with open("/opt/product.key", encoding="ascii") as key:
    headers = {"Authorization": f"Bearer {key.read().strip()}"}
body = httpx.get(url, params={"nonce": nonce.hex()}, headers=headers, timeout=30).raise_for_status().json()
verify_attestation_binding(
    AttestationResponse.from_dict(body), nonce, expected_account=account, expected_livemode=False
)
assert len(body["tdx_quote"]) > 0, "empty quote"
assert "operators" not in body, "the service sends no transactions, so it attests no operator"
keys = [{"version": key["version"], "public_key": key["public_key"]} for key in body["webhook_keys"]]
print(json.dumps({"webhook_keys": keys, "report_data": body["report_data"]}))
PY
)
echo "ok: GET /v1/attestation needs an API key and binds the nonce and the account's webhook key"
cli_attestation=$(dc exec -T topup topup attest --nonce "$nonce" --account "$account" |
    jq -c '{webhook_keys, report_data}')
[[ "$(jq -S . <<<"$cli_attestation")" == "$(jq -S . <<<"$attestation")" ]] ||
    die "topup attest and GET /v1/attestation disagree"
echo "ok: topup attest reports the same webhook key and report_data"

echo "== the reference-product CVM: rendered compose, unsealed env, public URL, then the sealed key"
driver_key=$(product_python -m topup_sdk keygen --keyid driver/v1 --seed-out /opt/driver.seed)
# The committed product compose with this network's addresses, as for the route above: its chains
# are this one Anvil Sepolia, with its treasury and test token.
chains=$(jq -cn --arg treasury "$treasury" --arg token "$token" \
    '[{chain_id: 11155111, name: "Sepolia", rpc_url: "http://anvil:8545", treasury: $treasury,
       test_tokens: [{symbol: "PHA", address: $token}]}]')
sed -e "s|^\(        \"factory\": \).*|\1\"$factory\",|" \
    -e "s|^\(        \"implementation\": \).*|\1\"$implementation\",|" \
    -e "s|^\(        \"account\": \).*|\1\"$account\",|" \
    "$root/deploy/product/docker-compose.yml" |
    awk -v chains="$chains" '
        /^        "chains": \[$/ { print "        \"chains\": " chains ","; skip = 1; next }
        skip { if ($0 ~ /^        \],$/) skip = 0; next }
        { print }
    ' >"$tmp/product-source.yml"
grep -Fq '"rpc_url": "http://anvil:8545"' "$tmp/product-source.yml" ||
    die "the product compose's chains were not replaced with the rehearsal's"
# render_product PUBLIC_URL: the settings Deploy (target `product`) renders, for this network.
render_product() {
    TOPUP_ORIGIN=http://topup:8080 PRODUCT_PUBLIC_URL=$1 \
        PRODUCT_DOMAIN=pay-demo-api.phala.com PRODUCT_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network \
        PRODUCT_DRIVER_PUBLIC_KEY="$(jq -er .public_key <<<"$driver_key")" \
        "$root/deploy/product/render-compose.sh" "$tmp/product-source.yml" >"$tmp/product.yml"
}
render_product https://pending.invalid
grep -Fq "\"factory\": \"$factory\"," "$tmp/product.yml" || die "the product compose lacks the rehearsal factory"
cat >"$tmp/product-overlay.yml" <<YAML
services:
  product:
    networks:
      default:
        aliases: [product]
  # As in topup's overlay (cvm-rehearsal.compose.yml): the custom domain needs a real CVM.
  dstack-ingress:
    profiles: [cvm]
networks:
  default:
    name: ${project}_default
    external: true
YAML
: >"$tmp/product.env"
env -i PATH="$PATH" "$root/deploy/write-staging-env.sh" --product "$tmp/product.env" >/dev/null
[[ "$(<"$tmp/product.env")" == PRODUCT_API_KEY= ]] || die "the unsealed product env is not only an empty key"
pc up -d >/dev/null
product_healthy() {
    [[ "$(http_status http://product:8089/healthz)" == 200 ]]
}
wait_for "the product's /healthz" 90 product_healthy
echo "ok: the unsealed product serves /healthz; it pins its webhook key once the key is sealed"
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
api_key=$(docker exec "$client" cat /opt/product.key)
sed -i "s/^PRODUCT_API_KEY=\$/PRODUCT_API_KEY=$api_key/" "$tmp/product.env"
unset api_key
pc up -d >/dev/null
wait_for "the product's /healthz after sealing" 90 product_healthy
jq -n --arg factory "$factory" --arg implementation "$implementation" --arg token "$token" \
    --arg payer "$owner" --arg treasury "$treasury" --arg account "$account" \
    '{service_url: "http://topup:8080", account: $account,
      factory: $factory, implementation: $implementation,
      chains: [{chain_id: 11155111, name: "Sepolia", rpc_url: "http://anvil:8545",
        treasury: $treasury, test_tokens: [{symbol: "PHA", address: $token}]}],
      public_url: "http://product:8089", payer: $payer}' |
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
