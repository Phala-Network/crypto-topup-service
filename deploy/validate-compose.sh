#!/bin/sh
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
compose=$(mktemp)
restore_check_compose=$(mktemp)
rendered=$(mktemp)
rendered_tools=$(mktemp)
compose_envs=$(mktemp)
allowed_envs=$(mktemp)
escaped_config=$(mktemp)
staging_envs=$(mktemp)
product_compose=$(mktemp)

cleanup() {
    rm -f "$compose" "$restore_check_compose" "$rendered" "$rendered_tools" "$compose_envs" "$allowed_envs" "$escaped_config" "$staging_envs" \
        "$product_compose"
}
trap cleanup EXIT INT TERM

# Both variants of the attested compose, rendered as Deploy does.
render() {
    TOPUP_IMAGE=ghcr.io/phala-network/phala-pay@sha256:1111111111111111111111111111111111111111111111111111111111111111 \
        POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:2222222222222222222222222222222222222222222222222222222222222222 \
        AWS_ENDPOINT=https://account.r2.cloudflarestorage.com AWS_REGION=auto \
        AWS_S3_FORCE_PATH_STYLE=false WALG_S3_PREFIX=s3://topup-staging/postgres \
        TOPUP_ADMIN_KID=staging-admin/v1 TOPUP_ADMIN_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo= \
        SENTRY_ENVIRONMENT=staging \
        TOPUP_DOMAIN=topup.example TOPUP_GATEWAY_DOMAIN=gateway.dstack.example \
        TOPUP_RPC_PROVIDER_A_URL=https://rpc-a.example \
        TOPUP_RPC_PROVIDER_B_URL=https://rpc-b.example "$root/deploy/render-compose.sh" "$@"
}
render >"$compose"
render --restore-check >"$restore_check_compose"

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

# The two variants differ only in the mode switches, the ingress, and the rendered-sha256 label.
# The service's only published port is dstack-ingress on 443 (tls-alpn-01, TLS for TOPUP_DOMAIN,
# which is the origin topup verifies signatures against). The restore-check variant runs no
# ingress and publishes only topup on 8081, so it never answers for the live domain and the gateway
# reaches it as `<app_id>-8081` (deploy/RESTORE.md).
ingress='def published: [.services | to_entries[] | select((.value.ports // []) | length > 0)
    | {service: .key, ports: .value.ports}];
    def only($service; $target; $port): published == [{service: $service, ports: [{
        mode: "ingress", target: $target, published: $port, protocol: "tcp"}]}];'
docker compose -f "$restore_check_compose" --profile tools config --format json |
    jq -e --slurpfile service "$rendered_tools" "$ingress"'
        def normal: del(.services[].labels) | del(.services["dstack-ingress"])
            | del(.volumes.ingress_certs, .volumes.ingress_evidences, .services.topup.ports)
            | (.services[] | select(.environment.TOPUP_SERVICE_ENABLED != null)
                | .environment.TOPUP_SERVICE_ENABLED) |= "on"
            | (.services[] | select(.environment.TOPUP_RESTORE_FROM_BACKUP != null)
                | .environment.TOPUP_RESTORE_FROM_BACKUP) |= "off";
        (.services.topup.environment.TOPUP_SERVICE_ENABLED == "read-only")
        and (.services.postgres.environment.TOPUP_RESTORE_FROM_BACKUP == "on")
        and only("topup"; 8080; "8081")
        and (normal == ($service[0] | normal))' >/dev/null || {
    echo "the restore-check variant must differ from the service only in its mode switches, in" \
        "running no dstack-ingress, and in publishing topup on 8081" >&2
    exit 1
}
jq -e "$ingress"'(.services.topup.environment.TOPUP_SERVICE_ENABLED == "on")
    and (.services.heartbeat.environment.TOPUP_SERVICE_ENABLED == "on")
    and ([.services[].environment.TOPUP_RESTORE_FROM_BACKUP // empty] | unique == ["off"])
    and only("dstack-ingress"; 443; "443")
    and (.services["dstack-ingress"].environment as $ingress
        | $ingress.CHALLENGE_TYPE == "tls-alpn-01" and $ingress.TARGET_ENDPOINT == "topup:8080"
        and .services.topup.environment.TOPUP_PUBLIC_ORIGIN == "https://\($ingress.DOMAIN)")' \
    "$rendered_tools" >/dev/null || {
    echo "the service variant must run the service, publish only dstack-ingress on 443 (tls-alpn-01," \
        "forwarding to topup:8080, serving the domain of TOPUP_PUBLIC_ORIGIN), and never" \
        "restore-check" >&2
    exit 1
}

# Least privilege by mount: the runtime services see only the application login's credentials.
jq -e '([.services.topup, .services.heartbeat | .volumes[]?.source]
        | any(. == "db_owner" or . == "walg_key") | not)
    and ([.services["dstack-ingress"].volumes[]?.source]
        | any(. == "db_owner" or . == "db_app" or . == "walg_key") | not)' \
    "$rendered_tools" >/dev/null || {
    echo "topup and heartbeat must mount neither db_owner nor walg_key, and dstack-ingress no" \
        "credentials" >&2
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

# The rendered compose reads only the owner-sealed secrets from the env.
docker compose -f "$compose" config --variables |
    awk 'NR > 1 && NF > 0 { print $1 }' |
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

# The local stack is an overlay on the rendered compose; check every combination the scripts use.
local_stack() {
    "$root/deploy/local/compose.sh" "$@" --profile tools config --format json
}
local_stack >"$rendered_tools"
local_stack -f "$root/deploy/sandbox/docker-compose.local.yml" >/dev/null
jq -e '[.services[].volumes[]? | select(.source == "/var/run/dstack.sock")] | length == 0' \
    "$rendered_tools" >/dev/null || {
    echo "the local overlay must replace the host dstack socket with the simulator's" >&2
    exit 1
}
local_stack --restore-check -f "$root/deploy/local/restore-drill.compose.yml" >"$rendered"
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

# The reference-product CVM (deploy/product), rendered as Deploy (target `product`) does: it reads
# exactly the names of its env example, which become its allowed_envs, carries its settings in the
# attested config, mounts no host path, and publishes only 8089.
PRODUCT_IMAGE=ghcr.io/phala-network/phala-pay-reference-product@sha256:3333333333333333333333333333333333333333333333333333333333333333 \
    TOPUP_ORIGIN=https://topup.example PRODUCT_PUBLIC_URL=https://product.example \
    PRODUCT_RPC_URL=https://rpc.example PRODUCT_DRIVER_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo= \
    "$root/deploy/product/render-compose.sh" >"$product_compose"
docker compose -f "$product_compose" config --variables |
    awk 'NR > 1 && NF > 0 { print $1 }' | sort >"$compose_envs"
awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' \
    "$root/deploy/product/staging.env.example" | sort -u >"$staging_envs"
cmp -s "$compose_envs" "$staging_envs" || {
    echo "the rendered product compose reads other variables than deploy/product/staging.env.example" >&2
    diff -u "$staging_envs" "$compose_envs" >&2 || true
    exit 1
}
docker compose -f "$product_compose" config --format json >"$rendered"
jq -e '([.services[].volumes[]? | select(.type == "bind")] | length == 0)
    and ([.services | to_entries[] | select((.value.ports // []) | length > 0) | .key] == ["product"])
    and ([.services.product.ports[].target] == [8089])
    and (.configs.product_config.content | fromjson
        | .service_url == "https://topup.example" and .rpc_url == "https://rpc.example")' \
    "$rendered" >/dev/null || {
    echo "the product compose must mount no host path, publish only product:8089, and carry its settings" >&2
    exit 1
}

echo "attested compose config, allowed_envs, and local overlay validation passed"
