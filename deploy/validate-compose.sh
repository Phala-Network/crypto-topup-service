#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
compose="$root/deploy/docker-compose.yml"
local_compose="$root/deploy/local/docker-compose.yml"
rendered=$(mktemp)
rendered_tools=$(mktemp)
compose_envs=$(mktemp)
allowed_envs=$(mktemp)
escaped_config=$(mktemp)
staging_envs=$(mktemp)

cleanup() {
    rm -f "$rendered" "$rendered_tools" "$compose_envs" "$allowed_envs" "$escaped_config" "$staging_envs"
}
trap cleanup EXIT INT TERM

docker compose -f "$compose" config --format json >"$rendered"
docker compose -f "$compose" --profile tools config --format json >"$rendered_tools"

if jq -e '.services.topup.environment | has("MIGRATE_DATABASE_URL")' "$rendered" \
    >/dev/null; then
    echo "topup must not receive MIGRATE_DATABASE_URL" >&2
    exit 1
fi

jq -e '
    .services.topup.command == [
        "topup",
        "run",
        "--bind",
        "0.0.0.0:8080",
        "--metrics-bind",
        "0.0.0.0:9464",
        "--route",
        "/etc/topup/routes/phala-cloud-sepolia-pha.yaml"
    ]
    and (.services.topup.environment | has("DATABASE_URL"))
    and (.services.topup.environment | has("TOPUP_ADMIN_KID"))
    and (.services.topup.environment | has("TOPUP_ADMIN_PUBLIC_KEY"))
    and (.services.topup.environment | has("TOPUP_PUBLIC_ORIGIN"))
    and (.services.topup.environment | has("TOPUP_RPC_PROVIDER_A_URL"))
' "$rendered" >/dev/null || {
    echo "topup command or required runtime environment is misconfigured" >&2
    exit 1
}

jq -e '
    .services["restore-check"].entrypoint == [
        "topup",
        "restore-check",
        "--route",
        "/etc/topup/routes/phala-cloud-sepolia-pha.yaml"
    ]
    and (.services["restore-check"].environment | has("MIGRATE_DATABASE_URL"))
    and (.services["restore-check"].volumes | any(.target == "/var/run/dstack.sock"))
    and (.services["restore-check"].environment | has("TOPUP_RPC_PROVIDER_A_URL"))
    and (.services["restore-check"].configs
        | any(.target == "/etc/topup/routes/phala-cloud-sepolia-pha.yaml"))
' "$rendered_tools" >/dev/null || {
    echo "restore-check must use owner credentials, the dstack socket, and the attested route" >&2
    exit 1
}

# Database credentials are derived in the CVM by `keys` and read from files; no environment may
# carry a database password.
jq -e '[.services[].environment // {} | to_entries[]
    | select((.key | test("PASSWORD$")) or ((.value // "") | test("postgres(ql)?://[^/@]*:[^/@]*@")))]
    | length == 0' "$rendered_tools" >/dev/null || {
    echo "a service environment carries a database password" >&2
    exit 1
}

# Least privilege by mount: the runtime services see only the application login's credentials.
jq -e '[.services.topup, .services.heartbeat | .volumes[]?.source]
    | any(. == "db_owner" or . == "walg_key") | not' "$rendered_tools" >/dev/null || {
    echo "topup and heartbeat must mount neither db_owner nor walg_key" >&2
    exit 1
}

compare_config() {
    name=$1
    path=$2
    if [ "${3:-}" = "escape-dollars" ]; then
        sed 's/[$]/&&/g' "$path" >"$escaped_config"
        path=$escaped_config
    fi
    jq -j --arg name "$name" '.configs[$name].content' "$rendered" | cmp -s - "$path" || {
        echo "attested config $name differs from $path" >&2
        return 1
    }
}

compare_config postgres_init_topup_role "$root/deploy/postgres-init/10-topup-role.sh" \
    escape-dollars
compare_config topup_route_phala_cloud_sepolia_pha \
    "$root/deploy/config/routes/phala-cloud-sepolia-pha.yaml"

docker compose -f "$compose" config --variables |
    awk 'NR > 1 && NF > 0 { print $1 }' |
    grep -Ev '^(POSTGRES_WALG_IMAGE|TOPUP_IMAGE)$' |
    sort >"$compose_envs"
jq -r '.allowed_envs[]' "$root/deploy/app-compose.example.json" | sort >"$allowed_envs"
cmp -s "$compose_envs" "$allowed_envs" || {
    echo "app-compose allowed_envs differs from compose secret variables" >&2
    diff -u "$compose_envs" "$allowed_envs" >&2 || true
    exit 1
}

awk '
/^[[:space:]]*($|#)/ { next }
{
    line = $0
    sub(/^[[:space:]]*export[[:space:]]+/, "", line)
    if (line !~ /^[A-Za-z_][A-Za-z0-9_]*=/) exit 64
    sub(/=.*/, "", line)
    print line
}
' "$root/deploy/staging.env.example" | sort -u >"$staging_envs"
cmp -s "$staging_envs" "$allowed_envs" || {
    echo "staging.env.example names differ from app-compose allowed_envs" >&2
    diff -u "$staging_envs" "$allowed_envs" >&2 || true
    exit 1
}

# The local stack is an overlay on the attested compose; check every combination the scripts use.
local_stack() {
    docker compose -f "$compose" -f "$local_compose" "$@" --profile tools config --format json
}
local_stack >"$rendered_tools"
local_stack -f "$root/deploy/sandbox/docker-compose.local.yml" |
    jq -e '[.services[].ports[]?.published] | index("19464") == null' >/dev/null || {
    echo "the sandbox stack must not publish the fixed metrics port" >&2
    exit 1
}
jq -e '[.services[].volumes[]? | select(.source == "/var/run/dstack.sock")] | length == 0' \
    "$rendered_tools" >/dev/null || {
    echo "the local overlay must replace the host dstack socket with the simulator's" >&2
    exit 1
}
local_stack -f "$root/deploy/local/restore-drill.compose.yml" >"$rendered"
jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' "$rendered" >/dev/null || {
    echo "the restore-drill stack bind-mounts a host path; CI's Docker daemon cannot see it" >&2
    exit 1
}
# Drills run concurrently with each other and with sandbox runs on one host.
jq -e '[.services[].ports[]?] | length == 0' "$rendered" >/dev/null || {
    echo "the restore-drill stack must not publish host ports" >&2
    exit 1
}

# The CVM rehearsal runs the rendered staging compose with only the simulator, S3, and Anvil
# added; like the drill it must not bind-mount, and only Anvil may publish a (chosen) port.
TOPUP_LOCAL_DSTACK_IMAGE=validate docker compose --project-directory "$root/deploy/local" \
    -f "$compose" -f "$root/deploy/local/cvm-rehearsal.compose.yml" config --format json \
    >"$rendered"
jq -e '[.services[].volumes[]? | select(.type == "bind")] | length == 0' "$rendered" >/dev/null || {
    echo "the CVM rehearsal stack bind-mounts a host path" >&2
    exit 1
}
jq -e '[.services | to_entries[] | select((.value.ports // []) | length > 0) | .key] == ["anvil"]' \
    "$rendered" >/dev/null || {
    echo "only Anvil may publish a port in the CVM rehearsal stack" >&2
    exit 1
}

# The reference-product CVM (deploy/product): it reads exactly the names of its env example, which
# become its allowed_envs, mounts no host path, and publishes only 8089.
product_compose="$root/deploy/product/docker-compose.yml"
docker compose -f "$product_compose" config --variables |
    awk 'NR > 1 && NF > 0 && $1 != "PRODUCT_IMAGE" { print $1 }' | sort >"$compose_envs"
awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' \
    "$root/deploy/product/staging.env.example" | sort -u >"$staging_envs"
cmp -s "$compose_envs" "$staging_envs" || {
    echo "the product compose reads other variables than deploy/product/staging.env.example" >&2
    diff -u "$staging_envs" "$compose_envs" >&2 || true
    exit 1
}
docker compose -f "$product_compose" config --format json >"$rendered"
jq -e '([.services[].volumes[]? | select(.type == "bind")] | length == 0)
    and ([.services | to_entries[] | select((.value.ports // []) | length > 0) | .key] == ["product"])
    and ([.services.product.ports[].target] == [8089])
    and (.configs.product_config.content | test("[$]") | not)' "$rendered" >/dev/null || {
    echo "the product compose must mount no host path and publish only product:8089" >&2
    exit 1
}

echo "attested compose config, allowed_envs, and local overlay validation passed"
