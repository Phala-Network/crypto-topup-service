#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
compose="$root/deploy/docker-compose.yml"
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
    and (.services.topup.environment | has("TOPUP_RPC_PROVIDER_A_URL"))
' "$rendered" >/dev/null || {
    echo "topup command or required runtime environment is misconfigured" >&2
    exit 1
}

jq -e '
    .services["restore-check"].command == ["topup", "restore-check"]
    and (.services["restore-check"].environment | has("RESTORE_DATABASE_URL"))
    and (.services["restore-check"].volumes | any(.target == "/var/run/dstack.sock"))
    and (.services["restore-check"].environment | has("TOPUP_RPC_PROVIDER_A_URL"))
    and (.services["restore-check"].configs
        | any(.target == "/etc/topup/routes/phala-cloud-sepolia-pha.yaml"))
' "$rendered_tools" >/dev/null || {
    echo "restore-check must use owner credentials, the dstack socket, and the attested route" >&2
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
compare_config topup_chain_ethereum_sepolia \
    "$root/deploy/config/chains/ethereum-sepolia.yaml"
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

echo "attested compose config and allowed_envs validation passed"
