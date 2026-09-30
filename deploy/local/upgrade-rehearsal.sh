#!/usr/bin/env bash
# The staging cutover in place, rehearsed (docs/design/deploy-config.md §11): the released stack
# with real data, upgraded to this checkout on its own volumes, then rolled back.
#
# 1. Starts the old deployment as a CVM runs it: the old images, pulled anonymously by digest
#    (staging's, of Deploy run 36670873413, by default), and the compose of the old commit rendered
#    by that commit's own renderer with a staging-shaped configuration: staging's routes, the
#    Anvils as their providers (their contracts at the routes' addresses), local object storage,
#    and a rehearsal admin key. It runs on the pinned Docker Compose v2.26.0 the CVM runs, under
#    one project, with sealed secrets.
# 2. Commits real data through the API: an account with its API key, webhook endpoint, treasury,
#    deposit address, and quote. Records the evidence: PostgreSQL's system identifier and timeline,
#    the migrations, the committed rows, the account's webhook key and deposit address, the derived
#    key files' digests, and the app identity.
# 3. Before the upgrade, asserts the project name, every volume's name and each service's mount
#    targets (running containers, and in both artifacts dstack-ingress's and the product's
#    `ledger`), PGDATA, the sealed names, and the backup prefix. The old artifact is kept.
# 4. Upgrades as Deploy does: this checkout's images, pushed to a loopback registry, and its
#    staging environment rendered by deploy/render.sh with the same substitutions, then
#    `docker compose up` on the same volumes.
# 5. After the upgrade, asserts the same system identifier and timeline, every committed row, the
#    migrations, the same webhook key, deposit address, client secret, and derived keys, a WAL
#    segment archived after the upgrade, a served attestation, and that no bootstrap or recovery
#    path ran: no base backup restored or cluster initialized, no recovery.signal, not in
#    recovery, and no restore recorded by topup.
# 6. Rolls back: the kept old artifact on the same volumes, with the same data.
#
# Everything it creates is removed on exit. Requires docker, Foundry v1.8.3 with contracts/lib
# checked out, jq, curl, and OpenSSL 3; internet access for the old images and the live price
# sources. UPGRADE_FROM_COMMIT, UPGRADE_FROM_TOPUP_IMAGE, and UPGRADE_FROM_POSTGRES_WALG_IMAGE
# select another release to upgrade from.
set -Eeuo pipefail
trap 'echo "upgrade-rehearsal: line $LINENO failed: $BASH_COMMAND" >&2' ERR

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in docker forge cast jq curl openssl git; do
    require_command "$command"
done

old_commit=${UPGRADE_FROM_COMMIT:-e39b06b5ee64c0c5985e2a441acd123918bfc83f}
old_topup=${UPGRADE_FROM_TOPUP_IMAGE:-ghcr.io/phala-network/phala-pay@sha256:4e02fd94b8a4d131a8afb04933ac60e510ac122b311098c241f7f782ea80f0fe}
old_postgres=${UPGRADE_FROM_POSTGRES_WALG_IMAGE:-ghcr.io/phala-network/postgres-walg@sha256:f8ad6c0947e349f4840841f13905bc7d9682bad72a301721ff8bc4fc12094eea}
git -C "$root" cat-file -e "$old_commit:deploy/render-compose.sh" 2>/dev/null ||
    die "$old_commit is not a commit with deploy/render-compose.sh"

project="topup-upgrade-rehearsal-$$"
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-upgrade-rehearsal.XXXXXX")
compose=$("$root/deploy/pinned-compose.sh")
registry="$project-registry"
registry_image="registry:3.1.1@sha256:325b4b29b041e82803abeb703e201655e4e23ab83264ec1a7c9ddb0a5b14a6e0"
owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
staging="$root/deploy/environments/phala-network/staging/topup"
export TOPUP_LOCAL_DSTACK_IMAGE="phala-pay-dstack-simulator:$project"
export SANDBOX_ANVIL_PORT REHEARSAL_BASE_SEPOLIA_ANVIL_PORT
SANDBOX_ANVIL_PORT=$(free_port)
REHEARSAL_BASE_SEPOLIA_ANVIL_PORT=$(free_port)
registry_port=$(free_port)
topup_port=$(free_port)
rpc_url="http://127.0.0.1:$SANDBOX_ANVIL_PORT"
base_rpc_url="http://127.0.0.1:$REHEARSAL_BASE_SEPOLIA_ANVIL_PORT"
api="http://127.0.0.1:$topup_port"
local_images=()
pulled_images=()

# The old rehearsal overlay and what it includes and extends, as of the old commit, so its relative
# paths resolve; the new overlay is this checkout's.
mkdir -p "$tmp/old/local" "$tmp/old/sandbox" "$tmp/cvm"
git -C "$root" show "$old_commit:deploy/local/cvm-rehearsal.compose.yml" >"$tmp/old/local/cvm-rehearsal.compose.yml"
git -C "$root" show "$old_commit:deploy/local/s3.compose.yml" >"$tmp/old/local/s3.compose.yml"
git -C "$root" show "$old_commit:deploy/sandbox/docker-compose.local.yml" >"$tmp/old/sandbox/docker-compose.local.yml"
# topup's port on loopback, so this script calls the API with curl.
printf 'services:\n  topup:\n    ports: ["127.0.0.1:%s:8080"]\n' "$topup_port" >"$tmp/port.yml"

# dc old|new ARGS...: Compose as the CVM runs it (the rendered file and its `.env`), plus the
# rehearsal overlay of that side and topup's loopback port. The sealed names are removed from the
# calling environment, so only the `.env` file supplies them.
dc() {
    local side=$1 overlay
    shift
    if [[ "$side" == old ]]; then
        overlay=(--project-directory "$tmp/old/local" -f "$tmp/cvm/old.yml"
            -f "$tmp/old/local/cvm-rehearsal.compose.yml")
    else
        overlay=(--project-directory "$root/deploy/local" -f "$tmp/cvm/new.yml"
            -f "$root/deploy/local/cvm-rehearsal.compose.yml")
    fi
    env -u AWS_ACCESS_KEY_ID -u AWS_SECRET_ACCESS_KEY -u SENTRY_DSN -u TOPUP_RPC_PROVIDER_A_KEY \
        -u TOPUP_RPC_PROVIDER_B_KEY "$compose" --progress quiet -p "$project" \
        --env-file "$tmp/cvm/.env" "${overlay[@]}" -f "$tmp/port.yml" "$@"
}

leftovers() {
    {
        docker ps -aq --filter "label=com.docker.compose.project=$project"
        docker volume ls -q --filter "label=com.docker.compose.project=$project"
        docker volume ls -q --filter "name=^${project}_"
        docker network ls -q --filter "label=com.docker.compose.project=$project"
        docker ps -aq --filter "name=^$registry\$"
    } 2>/dev/null
}
cleanup() {
    status=$?
    set +e
    if ((status != 0)) && [[ -f "$tmp/cvm/.side" ]]; then
        echo "--- topup and postgres logs (last 40 lines each) ---" >&2
        dc "$(<"$tmp/cvm/.side")" logs --no-color --tail 40 topup postgres >&2
    fi
    for side in new old; do
        [[ -f "$tmp/cvm/$side.yml" ]] && dc "$side" down --volumes --remove-orphans --timeout 10 >/dev/null 2>&1
    done
    docker rm -f -v "$registry" >/dev/null 2>&1
    docker image rm "${local_images[@]}" "${pulled_images[@]}" "$TOPUP_LOCAL_DSTACK_IMAGE" >/dev/null 2>&1
    rm -rf "$tmp"
    if [[ -n "$(leftovers)" ]]; then
        echo "FAIL: containers, volumes, or networks of $project were left behind:" >&2
        leftovers >&2
        status=1
    else
        echo "== shutdown left no container, volume, or network of $project"
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
healthy() {
    [[ "$(curl -s -o /dev/null -w '%{http_code}' "$api/healthz")" == 200 ]]
}
psql_value() {
    dc "$(<"$tmp/cvm/.side")" exec -T postgres psql -U postgres -d topup -XAtqc "$1"
}
# The API with the account's key: `merchant METHOD PATH [BODY]`.
merchant() {
    curl --fail-with-body -sS -X "$1" -H @"$tmp/auth.header" -H 'content-type: application/json' \
        ${3:+--data-binary "$3"} "$api$2"
}
# The signed admin API, for the origin topup verifies (http://topup:8080): `admin METHOD PATH`.
admin() {
    local headers
    : >"$tmp/admin-body"
    sleep 1
    mapfile -t headers < <("$root/deploy/runbooks/sign-admin-request.sh" "$1" \
        "http://topup:8080$2" "$tmp/admin-body" "$tmp/admin.pem" admin/staging-v1)
    curl --fail-with-body -sS -X "$1" "${headers[@]/#/-H}" "$api$2"
}

echo "== the old images, pulled anonymously, and this checkout's images in a loopback registry"
for image in "$old_topup" "$old_postgres"; do
    docker image inspect "$image" >/dev/null 2>&1 || pulled_images+=("$image")
    docker pull --quiet "$image" >/dev/null
done
docker run -d --name "$registry" -p "127.0.0.1:$registry_port:5000" \
    --mount type=tmpfs,destination=/var/lib/registry "$registry_image" >/dev/null
wait_for "the registry" 30 curl -fsS "http://127.0.0.1:$registry_port/v2/"
export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
# publish NAME BUILD_ARGS...: builds, pushes, and prints the repository@sha256 reference.
publish() {
    local tag="127.0.0.1:$registry_port/$1:upgrade" digest
    shift
    docker build --quiet --build-arg "SOURCE_DATE_EPOCH=$SOURCE_DATE_EPOCH" \
        --build-arg "BUILD_JOBS=${CARGO_BUILD_JOBS:-}" -t "$tag" "$@" >/dev/null
    local_images+=("$tag")
    docker push --quiet "$tag" >/dev/null
    digest=$(docker image inspect --format '{{json .RepoDigests}}' "$tag" |
        jq -er --arg repository "${tag%:upgrade}" '.[] | select(startswith($repository + "@"))')
    local_images+=("$digest")
    printf '%s' "$digest"
}
new_topup=$(publish phala-pay "$root")
new_postgres=$(publish postgres-walg -f "$root/deploy/Dockerfile.postgres-walg" "$root")
jq -n --arg topup "$new_topup" --arg postgres "$new_postgres" \
    '{"phala-pay": $topup, "postgres-walg": $postgres}' >"$tmp/images.json"
docker build --quiet -t "$TOPUP_LOCAL_DSTACK_IMAGE" \
    -f "$root/deploy/local/Dockerfile.dstack-simulator" "$root" >/dev/null
printf 'old: %s %s\nnew: %s %s\n' "$old_topup" "$old_postgres" "$new_topup" "$new_postgres"

echo "== rendering the old and the new artifact with the same settings"
openssl genpkey -algorithm ed25519 -out "$tmp/admin.pem"
admin_public_key=$(openssl pkey -in "$tmp/admin.pem" -pubout -outform DER | tail -c 32 | base64)
# The old commit's compose and renderer, with the settings Deploy rendered for staging, but the
# Anvils as providers, local object storage, and the rehearsal's admin key.
git -C "$root" show "$old_commit:deploy/docker-compose.yml" >"$tmp/old/source.yml"
git -C "$root" show "$old_commit:deploy/staging.env.example" >"$tmp/old/staging.env.example"
git -C "$root" show "$old_commit:deploy/render-compose.sh" >"$tmp/old/render-compose.sh"
env TOPUP_IMAGE="$old_topup" POSTGRES_WALG_IMAGE="$old_postgres" \
    AWS_ENDPOINT=http://s3:3900 AWS_REGION=us-east-1 AWS_S3_FORCE_PATH_STYLE=true \
    WALG_S3_PREFIX=s3://topup-backups/postgres TOPUP_ADMIN_KID=admin/staging-v1 \
    TOPUP_ADMIN_PUBLIC_KEY="$admin_public_key" SENTRY_ENVIRONMENT=staging \
    TOPUP_DOMAIN=pay-api-staging.phala.com TOPUP_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network \
    TOPUP_RPC_PROVIDER_A_URL=http://anvil:8545 TOPUP_RPC_PROVIDER_B_URL='http://anvil:8545/?key={key}' \
    TOPUP_RPC_BASE_SEPOLIA_A_URL=http://anvil-base-sepolia:8545 \
    TOPUP_RPC_BASE_SEPOLIA_B_URL='http://anvil-base-sepolia:8545/?provider=b' \
    bash "$tmp/old/render-compose.sh" --env-example "$tmp/old/staging.env.example" \
    "$tmp/old/source.yml" >"$tmp/cvm/old.yml"
# This checkout's staging environment with the same substitutions.
mkdir "$tmp/environment"
sed -e 's|WALG_S3_PREFIX: .*|WALG_S3_PREFIX: s3://topup-backups/postgres|' \
    -e 's|AWS_ENDPOINT: .*|AWS_ENDPOINT: http://s3:3900|' -e 's|AWS_REGION: .*|AWS_REGION: us-east-1|' \
    "$staging/compose.yaml" >"$tmp/environment/compose.yaml"
docker run --rm -i --network none "$new_topup" topup config show /dev/stdin <"$staging/topup.yaml" |
    jq --arg key "$admin_public_key" '.admin_key.public_key = $key
        | .rpc_providers = {"provider-a": "http://anvil:8545", "provider-b": "http://anvil:8545/?key={key}",
            "base-sepolia-a": "http://anvil-base-sepolia:8545",
            "base-sepolia-b": "http://anvil-base-sepolia:8545/?provider=b"}' >"$tmp/environment/topup.yaml"
"$root/deploy/render.sh" --images "$tmp/images.json" \
    --gateway-domain gateway.dstack-pha-prod5.phala.network --project-name "$project" \
    "$tmp/environment" >"$tmp/cvm/new.yml"
cp "$tmp/cvm/old.yml" "$tmp/old-artifact.yml"

# The sealed names, the same in both artifacts, sealed as the owner seals them: provider B's key.
sealed() {
    "$compose" -f "$1" config --variables | awk 'NR > 1 && NF > 0 { print $1 }' | sort
}
[[ "$(sealed "$tmp/cvm/old.yml")" == "$(sealed "$tmp/cvm/new.yml")" ]] ||
    die "the sealed names differ: $(sealed "$tmp/cvm/old.yml" | tr '\n' ' ') / $(sealed "$tmp/cvm/new.yml" | tr '\n' ' ')"
printf '%s\n' AWS_ACCESS_KEY_ID=topup-s3 AWS_SECRET_ACCESS_KEY=topup-s3-secret-key SENTRY_DSN= \
    TOPUP_RPC_PROVIDER_A_KEY= TOPUP_RPC_PROVIDER_B_KEY=rehearsal-rpc-key >"$tmp/cvm/.env"
[[ "$(cut -d= -f1 "$tmp/cvm/.env" | sort)" == "$(sealed "$tmp/cvm/new.yml")" ]] ||
    die "the rehearsal .env is not the sealed names"
echo "ok: both artifacts read exactly the sealed names $(sealed "$tmp/cvm/new.yml" | tr '\n' ' ')"

# layout FILE: every service's mounts and every volume's name, for the CVM's project name.
layout() {
    "$compose" -p dstack -f "$1" config --no-interpolate --format json | jq -S '{
        volumes: (.volumes | map_values(.name)),
        mounts: (.services | map_values([.volumes[]? | {type, source, target}] | sort_by(.target)))}'
}
# The service variant of the new artifact under the CVM's project, for the static comparison.
"$root/deploy/render.sh" --images "$tmp/images.json" --gateway-domain gateway.dstack-pha-prod5.phala.network \
    "$tmp/environment" >"$tmp/new-dstack.yml"
layout "$tmp/old-artifact.yml" | jq 'del(.mounts["restore-check", "restore"])' >"$tmp/old-layout.json"
layout "$tmp/new-dstack.yml" >"$tmp/new-layout.json"
cmp -s "$tmp/old-layout.json" "$tmp/new-layout.json" || {
    diff -u "$tmp/old-layout.json" "$tmp/new-layout.json" >&2
    die "the new artifact changes a volume name or a mount (dstack-ingress's included)"
}
jq -e '.volumes.pgdata == "dstack_pgdata" and .volumes.ingress_certs == "dstack_ingress_certs"
    and (.mounts["dstack-ingress"] | map(.target) | index("/etc/letsencrypt")) != null' \
    "$tmp/new-layout.json" >/dev/null || die "the CVM's volumes are not the ones it has today"
# The product's ledger: the old product compose, rendered by its renderer, against its environment.
git -C "$root" show "$old_commit:deploy/product/docker-compose.yml" >"$tmp/old/product.yml"
git -C "$root" show "$old_commit:deploy/product/staging.env.example" >"$tmp/old/product.env.example"
env PRODUCT_IMAGE="ghcr.io/phala-network/phala-pay-reference-product@sha256:$(printf '3%.0s' {1..64})" \
    TOPUP_ORIGIN=https://pay-api-staging.phala.com PRODUCT_PUBLIC_URL=https://pay-demo-api.phala.com \
    PRODUCT_DOMAIN=pay-demo-api.phala.com PRODUCT_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network \
    PRODUCT_DRIVER_PUBLIC_KEY=VRrLsnYWLAu6DiFfVKuem88c20EyOKE5v3CCTidUHQM= \
    bash "$tmp/old/render-compose.sh" --env-example "$tmp/old/product.env.example" \
    "$tmp/old/product.yml" >"$tmp/old-product.yml"
jq -n '{"phala-pay-reference-product": "ghcr.io/phala-network/phala-pay-reference-product@sha256:\("3" * 64)"}' \
    >"$tmp/product-images.json"
"$root/deploy/render.sh" --images "$tmp/product-images.json" \
    --gateway-domain gateway.dstack-pha-prod5.phala.network \
    "$root/deploy/environments/phala-network/staging/product" >"$tmp/new-product.yml"
cmp -s <(layout "$tmp/old-product.yml") <(layout "$tmp/new-product.yml") ||
    die "the new product artifact changes a volume name or a mount"
layout "$tmp/new-product.yml" | jq -e '.volumes.ledger == "dstack_ledger"
    and .mounts.product == [{type: "volume", source: "ledger", target: "/data"}]' >/dev/null ||
    die "the product's ledger is not the volume it has today"
echo "ok: every volume name and mount target is unchanged (topup, dstack-ingress, and the product's ledger)"

echo "== the old deployment: Anvil with the routes' contracts, then the stack, sealed"
echo old >"$tmp/cvm/.side"
dc old up -d --wait anvil anvil-base-sepolia >/dev/null
install_anvil_multicall3 "$rpc_url"
install_anvil_multicall3 "$base_rpc_url"
export FOUNDRY_BROADCAST="$tmp/broadcast"
# deploy_chain RPC_URL NETWORK CHAIN_ID: the deterministic factory, and the mock token and
# sanctions oracle's code at the staging routes' addresses of that chain.
deploy_chain() {
    local rpc=$1 network=$2 chain_id=$3 factory token oracle
    "$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$rpc" --local-fund --broadcast >/dev/null
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" \
        --rpc "$network/a=$rpc" --broadcast >/dev/null 2>&1 || die "deploy-factory.sh failed on $network"
    factory=$("$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" --rpc "$network/a=$rpc" | jq -er '.chains[0].factory')
    "$root/deploy/sandbox/deploy-test-contracts.sh" --anvil-unlocked "$owner" --rpc-url "$rpc" \
        >"$tmp/contracts-$chain_id.json"
    token=$(cast code "$(jq -er .test_token "$tmp/contracts-$chain_id.json")" --rpc-url "$rpc")
    oracle=$(cast code "$(jq -er .sanctions_oracle "$tmp/contracts-$chain_id.json")" --rpc-url "$rpc")
    jq -r --argjson chain "$chain_id" '.routes[] | select(.chain.chain_id == $chain)
        | [.chain.forwarder_factory, .asset.contract, .chain.sanctions_oracle] | @tsv' \
        "$tmp/environment/topup.yaml" | while IFS=$'\t' read -r route_factory contract route_oracle; do
        [[ "${route_factory,,}" == "${factory,,}" ]] || die "the deterministic factory is not the routes' on $network"
        cast rpc anvil_setCode "$contract" "$token" --rpc-url "$rpc" >/dev/null
        cast rpc anvil_setCode "$route_oracle" "$oracle" --rpc-url "$rpc" >/dev/null
    done
}
deploy_chain "$rpc_url" sepolia 11155111
deploy_chain "$base_rpc_url" base-sepolia 84532
dc old up -d --remove-orphans >/dev/null 2>&1 || true
wait_for "the old service's /healthz" 150 healthy
echo "ok: the old deployment serves /healthz"

echo "== committing real data through the API"
jq -cjn '{name: "upgrade-rehearsal", contact: {name: "Rehearsal", email: "rehearsal@example.com"},
      due_diligence: {reference: "upgrade", reviewed_at: "2026-09-30", reviewed_by: "upgrade-rehearsal"},
      charges_enabled: false, reason: "upgrade rehearsal"}' >"$tmp/admin-body.json"
mapfile -t headers < <("$root/deploy/runbooks/sign-admin-request.sh" POST \
    http://topup:8080/v1/admin/accounts "$tmp/admin-body.json" "$tmp/admin.pem" admin/staging-v1)
curl --fail-with-body -sS -X POST "${headers[@]/#/-H}" -H 'content-type: application/json' \
    --data-binary @"$tmp/admin-body.json" "$api/v1/admin/accounts" >"$tmp/account.json"
account=$(jq -er .id "$tmp/account.json")
(umask 077 && jq -jer '.api_keys[0].secret' "$tmp/account.json" >"$tmp/key" &&
    printf 'authorization: Bearer %s\n' "$(<"$tmp/key")" >"$tmp/auth.header")
merchant POST /v1/webhook_endpoints \
    '{"url": "https://merchant.example.com/phala-pay/webhooks", "enabled_events": ["*"]}' >/dev/null
wait_for "the sanctions oracle at the finalized block" 60 \
    cast call "$(jq -r '[.routes[] | select(.chain.chain_id == 11155111)][0].chain.sanctions_oracle' \
        "$tmp/environment/topup.yaml")" 'isSanctioned(address)(bool)' "$owner" --block finalized --rpc-url "$rpc_url"
"$root/deploy/sandbox/set-treasury.sh" --api "$api" --key-file "$tmp/key" --chain-id 11155111 \
    --private-key "$ANVIL_PRIVATE_KEY" >"$tmp/treasury.json"
merchant POST /v1/deposit_addresses '{"client_reference_id": "upgrade-rehearsal"}' >"$tmp/deposit-address.json"
deposit_address=$(jq -er .id "$tmp/deposit-address.json")
merchant POST /v1/quotes '{"client_reference_id": "upgrade-rehearsal", "amount": 2500, "currency": "usd",
    "chain_id": 11155111, "asset": "pha"}' >"$tmp/quote.json"
quote=$(jq -er .id "$tmp/quote.json")
echo "ok: account $account, its key, webhook endpoint, treasury, deposit address $deposit_address, and quote $quote"

# evidence FILE: what must survive the upgrade.
evidence() {
    local side key_digests
    side=$(<"$tmp/cvm/.side")
    # PostgreSQL mounts all three key volumes; the distroless `keys` image has no sha256sum.
    key_digests=$(dc "$side" exec -T postgres sha256sum /run/wal-g/backup.key \
        /run/db-owner/postgres.pgpass /run/db-app/topup_service.pgpass | awk '{ print $1 }' |
        paste -sd, -)
    jq -n \
        --arg system "$(psql_value 'SELECT system_identifier FROM pg_control_system()')" \
        --arg timeline "$(psql_value 'SELECT timeline_id FROM pg_control_checkpoint()')" \
        --arg migrations "$(psql_value 'SELECT max(version) || ":" || count(*) FROM _sqlx_migrations WHERE success')" \
        --arg rows "$(psql_value "SELECT string_agg(t || '=' || n, ',' ORDER BY t) FROM (
            SELECT 'accounts' t, string_agg(id::text, ' ' ORDER BY id) n FROM accounts
            UNION ALL SELECT 'api_keys', string_agg(id::text, ' ' ORDER BY id) FROM api_keys
            UNION ALL SELECT 'treasuries', string_agg(id::text, ' ' ORDER BY id) FROM treasuries
            UNION ALL SELECT 'webhook_endpoints', string_agg(id::text, ' ' ORDER BY id) FROM webhook_endpoints
            UNION ALL SELECT 'deposit_addresses', string_agg(id::text, ' ' ORDER BY id) FROM deposit_addresses
            UNION ALL SELECT 'quotes', string_agg(id::text, ' ' ORDER BY id) FROM quotes) r")" \
        --arg keys "$key_digests" \
        --argjson webhook_keys "$(merchant GET "/v1/attestation?nonce=00ff" | jq -c .webhook_keys)" \
        --argjson deposit_address "$(merchant GET "/v1/deposit_addresses/$deposit_address" |
            jq -c '{address, networks, salt, client_secret}')" \
        --argjson quote "$(merchant GET "/v1/quotes/$quote" | jq -c '{address, amount_atomic, exchange_rate, expires_at, client_secret}')" \
        --argjson identity "$(dc "$side" exec -T topup topup attest --nonce 00 --account "$account" |
            jq -c '{app_id, webhook_keys}')" \
        '$ARGS.named' >"$1"
}
evidence "$tmp/before.json"
jq . "$tmp/before.json"
# The running containers' mounts, and PGDATA and the backup prefix.
running() {
    local side id
    side=$(<"$tmp/cvm/.side")
    for id in $(dc "$side" ps -q); do
        docker inspect "$id" --format '{{json .}}'
    done | jq -s -S '[.[] | {service: .Config.Labels["com.docker.compose.service"],
        project: .Config.Labels["com.docker.compose.project"],
        mounts: ([.Mounts[] | {type: .Type, name: (.Name // .Source), target: .Destination}] | sort_by(.target)),
        pgdata: ([.Config.Env[] | select(startswith("PGDATA=") or startswith("WALG_S3_PREFIX="))] | sort)}]
        | map(select(.service != "anvil" and .service != "anvil-base-sepolia" and .service != "s3"
            and .service != "s3-init" and .service != "dstack-simulator"))
        | sort_by(.service)'
}
running >"$tmp/running-before.json"
jq -e --arg project "$project" 'all(.[]; .project == $project)
    and (map(select(.service == "postgres"))[0].mounts | any(.name == "\($project)_pgdata" and .target == "/var/lib/postgresql"))
    and (map(select(.service == "postgres"))[0].pgdata | index("PGDATA=/var/lib/postgresql/data")) != null
    and (map(select(.service == "postgres"))[0].pgdata | index("WALG_S3_PREFIX=s3://topup-backups/postgres")) != null' \
    "$tmp/running-before.json" >/dev/null || die "the old deployment is not laid out as expected"

echo "== upgrading in place, as Deploy upgrades"
upgrade_at=$(date +%s)
echo new >"$tmp/cvm/.side"
dc new up -d --remove-orphans >/dev/null 2>&1 || true
wait_for "the upgraded service's /healthz" 150 healthy
running >"$tmp/running-after.json"
# The same project, volumes, mounts, PGDATA, and backup prefix; restore-check and the old tools
# service never run in the service variant.
cmp -s <(jq 'map(del(.pgdata))' "$tmp/running-before.json") <(jq 'map(del(.pgdata))' "$tmp/running-after.json") ||
    { diff -u "$tmp/running-before.json" "$tmp/running-after.json" >&2; die "the upgrade changed a running container's mounts"; }
cmp -s "$tmp/running-before.json" "$tmp/running-after.json" ||
    { diff -u "$tmp/running-before.json" "$tmp/running-after.json" >&2; die "the upgrade changed PGDATA or the backup prefix"; }
echo "ok: the upgrade kept the project, every volume and mount, PGDATA, and the backup prefix"
postgres_id=$(dc new ps -q postgres)
# No bootstrap or recovery path ran: the entrypoint restores an empty data directory from backup
# (deploy/scripts/postgres-walg-entrypoint.sh), which would look healthy while losing data.
if docker logs "$postgres_id" 2>&1 | grep -E 'restoring base backup|initializing a new cluster|the backup prefix holds no base backup|starting archive recovery'; then
    die "PostgreSQL ran a bootstrap or recovery path after the upgrade"
fi
dc new exec -T postgres test ! -e /var/lib/postgresql/data/recovery.signal ||
    die "the upgraded data directory carries recovery.signal"
[[ "$(psql_value 'SELECT pg_is_in_recovery()')" == f ]] || die "PostgreSQL is in recovery after the upgrade"
admin GET /v1/admin/restore | jq -e '.frozen == false' >/dev/null ||
    die "topup recorded a restore after the upgrade"
evidence "$tmp/after.json"
cmp -s <(jq -S . "$tmp/before.json") <(jq -S . "$tmp/after.json") || {
    diff -u <(jq -S . "$tmp/before.json") <(jq -S . "$tmp/after.json") >&2
    die "the upgrade changed committed data or an identity"
}
echo "ok: the same database (system identifier, timeline), rows, migrations, keys, and identities"
archived_after_upgrade() {
    local marker
    marker=$(dc new exec -T backup cat /run/topup-observability/last-backup-unix-seconds) &&
        ((marker > upgrade_at))
}
wait_for "a WAL segment archived after the upgrade" 90 archived_after_upgrade
merchant GET "/v1/attestation?nonce=00ff" | jq -e '(.tdx_quote | length) > 0' >/dev/null ||
    die "the upgraded service serves no attestation"
echo "ok: WAL archiving continues, and the upgraded service is attested"

echo "== rolling back to the kept old artifact on the same volumes"
cp "$tmp/old-artifact.yml" "$tmp/cvm/old.yml"
echo old >"$tmp/cvm/.side"
dc old up -d --remove-orphans >/dev/null 2>&1 || true
wait_for "the rolled-back service's /healthz" 150 healthy
evidence "$tmp/rolled-back.json"
cmp -s <(jq -S . "$tmp/before.json") <(jq -S . "$tmp/rolled-back.json") || {
    diff -u <(jq -S . "$tmp/before.json") <(jq -S . "$tmp/rolled-back.json") >&2
    die "the rollback changed committed data or an identity"
}
echo "ok: the rollback serves the same data and identities"
echo "upgrade-rehearsal: all checks passed"
