#!/usr/bin/env bash
# deploy/render.sh: its three deploy-time inputs and their formats, digest-named configs (a changed
# file changes exactly the services that mount it), a reproducible output, and
# deploy/compose-policy.jq refusing an environment overlay that breaks it.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
compose=$("$root/deploy/pinned-compose.sh")
staging="$root/deploy/environments/phala-network/staging/topup"
gateway=(--gateway-domain gateway.dstack-pha-prod5.phala.network)
origin=(--restore-check --origin https://0123abcd-8081.dstack-pha-prod5.phala.network)

images() {
    jq -n --arg topup "$1" '{"phala-pay": $topup,
        "postgres-walg": "ghcr.io/phala-network/postgres-walg@sha256:\("2" * 64)",
        "phala-pay-reference-product": "ghcr.io/phala-network/phala-pay-reference-product@sha256:\("3" * 64)"}'
}
topup=ghcr.io/phala-network/phala-pay@sha256:$(printf '1%.0s' {1..64})
images "$topup" >"$tmp/images.json"
render() {
    "$root/deploy/render.sh" --images "$tmp/images.json" "$@"
}
# refused NAME MESSAGE ARGS...: render.sh fails with MESSAGE on stderr.
refused() {
    local name=$1 message=$2
    shift 2
    if "$root/deploy/render.sh" "$@" >"$tmp/$name.out" 2>"$tmp/$name.err"; then
        echo "render.sh accepted $name" >&2
        exit 1
    fi
    grep -F -- "$message" "$tmp/$name.err" >/dev/null || {
        echo "render.sh refused $name for an unexpected reason:" >&2
        cat "$tmp/$name.err" >&2
        exit 1
    }
}

render "${gateway[@]}" "$staging" >"$tmp/service.yml"
render "${gateway[@]}" "$staging" | cmp -s - "$tmp/service.yml" ||
    { echo "two renders of the same inputs differ" >&2; exit 1; }
grep -F "image: $topup" "$tmp/service.yml" >/dev/null
"$compose" -f "$tmp/service.yml" config --no-interpolate --format json >"$tmp/service.json"
# Every config is inline content named after its digest, and the services mount it by that name.
jq -e '(.configs | keys | all(test("^(postgres_init|topup)_[0-9a-f]{12}$")))
    and ([.services[].configs[]?.source] | unique) == (.configs | keys)' "$tmp/service.json" >/dev/null
# Its content is the committed script with every `$` escaped for Compose, as Compose prints it.
jq -j '.configs | to_entries[] | select(.key | startswith("postgres_init_")) | .value.content' \
    "$tmp/service.json" | cmp -s - <(sed 's/[$]/&&/g' "$root/deploy/postgres-init/10-topup-role.sh") ||
    { echo "the init script is not the committed one" >&2; exit 1; }

# A changed topup.yaml changes the definition of exactly the services that mount it.
cp -r "$staging" "$tmp/changed"
sed -i 's|id: admin/staging-v1|id: admin/staging-v2|' "$tmp/changed/topup.yaml"
render "${gateway[@]}" "$tmp/changed" >"$tmp/changed.yml"
changed=$({ diff <("$compose" -f "$tmp/service.yml" config --hash '*') \
    <("$compose" -f "$tmp/changed.yml" config --hash '*') || true; } | awk '/^>/ { print $2 }' |
    tr '\n' ' ')
[[ "$changed" == "topup " ]] || { echo "a changed topup.yaml changed: $changed" >&2; exit 1; }

# The inputs: formats, and each only in its variant.
images phala-pay:latest >"$tmp/bare.json"
refused bare-tag "--images must map image names" --images "$tmp/bare.json" "${gateway[@]}" "$staging"
images "ghcr.io/phala-network/phala-pay@sha256:$(printf '0%.0s' {1..64})" >"$tmp/zero.json"
refused zero-digest "--images must map image names" --images "$tmp/zero.json" "${gateway[@]}" "$staging"
jq 'del(.["postgres-walg"])' "$tmp/images.json" >"$tmp/missing.json"
refused unpinned "every image must be a nonzero repository@sha256 digest" \
    --images "$tmp/missing.json" "${gateway[@]}" "$staging"
refused no-gateway "needs --gateway-domain HOST" --images "$tmp/images.json" "$staging"
refused bad-gateway "needs --gateway-domain HOST" --images "$tmp/images.json" \
    --gateway-domain 'gw.example"x' "$staging"
refused origin-in-service "needs --gateway-domain HOST (and no --origin)" --images "$tmp/images.json" \
    "${gateway[@]}" --origin https://x.example.net "$staging"
refused http-origin "--restore-check needs --origin https://HOST" --images "$tmp/images.json" \
    --restore-check --origin http://x.example.net "$staging"
refused gateway-in-restore-check "--restore-check needs --origin" --images "$tmp/images.json" \
    "${origin[@]}" "${gateway[@]}" "$staging"
render "${origin[@]}" "$staging" >"$tmp/restore-check.yml"
refused product-restore-check "--restore-check and --template render a topup environment only" \
    --images "$tmp/images.json" "${origin[@]}" "$root/deploy/environments/phala-network/staging/product"

# overlay NAME YAML: staging's environment with YAML appended to its overlay.
overlay() {
    cp -r "$staging" "$tmp/$1"
    printf '%s\n' "$2" >>"$tmp/$1/compose.yaml"
}
# A sealed value may fill only its own environment key: never a setting, a command, or a config.
overlay secret-domain '  sentry-leak:
    image: busybox@sha256:'"$(printf '4%.0s' {1..64})"'
    environment:
      DOMAIN: ${AWS_SECRET_ACCESS_KEY:-}'
refused secret-domain "a sealed value may not fill services.sentry-leak.environment.DOMAIN" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/secret-domain"
# A `$` in topup.yaml is escaped: it never becomes an interpolated reference.
cp -r "$staging" "$tmp/dollar"
sed -i 's|^environment: staging$|environment: staging # ${SENTRY_DSN:-x}|' "$tmp/dollar/topup.yaml"
render "${gateway[@]}" "$tmp/dollar" >"$tmp/dollar.yml"
grep -F 'environment: staging # $${SENTRY_DSN:-x}' "$tmp/dollar.yml" >/dev/null ||
    { echo "render.sh did not escape a \$ in the configuration" >&2; exit 1; }
cmp -s <("$compose" -f "$tmp/service.yml" config --variables | sort) \
    <("$compose" -f "$tmp/dollar.yml" config --variables | sort) ||
    { echo "a \$ in topup.yaml became an interpolated reference" >&2; exit 1; }
cp -r "$staging" "$tmp/secret-rpc"
cat >>"$tmp/secret-rpc/compose.yaml" <<'YAML'
  migrate:
    environment:
      TOPUP_RPC_PROVIDER_A_KEY: ${TOPUP_RPC_PROVIDER_A_KEY:-}
YAML
refused secret-rpc "a sealed value may not fill services.migrate.environment.TOPUP_RPC_PROVIDER_A_KEY" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/secret-rpc"
cp -r "$staging" "$tmp/secret-command"
cat >>"$tmp/secret-command/compose.yaml" <<'YAML'
  heartbeat:
    command: [topup, heartbeat, --interval-s, "${SENTRY_DSN:-60}"]
YAML
refused secret-command "a sealed value may not fill services.heartbeat.command" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/secret-command"
# Topology an environment may not change.
cp -r "$staging" "$tmp/extra-port"
cat >>"$tmp/extra-port/compose.yaml" <<'YAML'
  migrate:
    ports: ["5432:5432"]
YAML
refused extra-port "only dstack-ingress may publish a port, 443" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/extra-port"
cp -r "$staging" "$tmp/env-file"
cat >>"$tmp/env-file/compose.yaml" <<'YAML'
  heartbeat:
    env_file: [/dstack/.host-shared/.decrypted-env]
YAML
refused env-file "no service may build, read an env_file, extend, or carry a profile" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/env-file"
cp -r "$staging" "$tmp/socket"
cat >>"$tmp/socket/compose.yaml" <<'YAML'
  migrate:
    volumes: [/var/run/dstack.sock:/var/run/dstack.sock]
YAML
refused socket "only keys, topup, and dstack-ingress may mount the dstack socket" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/socket"
# Smokescreen's deny list is exact: a dropped range is refused.
cp -r "$staging" "$tmp/smokescreen"
cat >>"$tmp/smokescreen/compose.yaml" <<'YAML'
  smokescreen:
    command: [smokescreen, --listen-ip=0.0.0.0, --listen-port=4750, --timeout=10s]
YAML
refused smokescreen "smokescreen must run its exact deny list from the service image" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/smokescreen"
# Each credential volume has exactly its committed mounters, read-only but for keys, on tmpfs.
cp -r "$staging" "$tmp/extra-mounter"
cat >>"$tmp/extra-mounter/compose.yaml" <<'YAML'
  heartbeat:
    volumes: ["walg_key:/run/wal-g:ro"]
YAML
refused extra-mounter "walg_key must be mounted by exactly backup, keys, postgres" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/extra-mounter"
cp -r "$staging" "$tmp/writable-mounter"
cat >>"$tmp/writable-mounter/compose.yaml" <<'YAML'
  heartbeat:
    volumes: ["db_app:/run/db-app"]
YAML
refused writable-mounter "only keys may mount db_app writable" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/writable-mounter"
cp -r "$staging" "$tmp/on-disk"
cat >>"$tmp/on-disk/compose.yaml" <<'YAML'
volumes:
  db_owner:
    driver_opts: !reset {}
YAML
refused on-disk "db_owner must be a tmpfs volume (uid=999,gid=999,mode=0700)" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/on-disk"
cp -r "$staging" "$tmp/no-archive"
sed -i 's|^      AWS_S3_FORCE_PATH_STYLE: "true"$|&\n      TOPUP_RESTORE_FROM_BACKUP: "on"|' \
    "$tmp/no-archive/compose.yaml"
refused no-archive "the service must archive" --images "$tmp/images.json" "${gateway[@]}" \
    "$tmp/no-archive"
cp -r "$staging" "$tmp/other-domain"
sed -i 's|DOMAIN: pay-api-staging.phala.com|DOMAIN: other.phala.com|' "$tmp/other-domain/compose.yaml"
refused other-domain "dstack-ingress must serve the host of topup's public_origin" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/other-domain"
# The restore-check variant never reads the live storage credentials.
cp -r "$staging" "$tmp/live-credentials"
cat >>"$tmp/live-credentials/compose.yaml" <<'YAML'
  migrate:
    environment:
      AWS_ACCESS_KEY_ID: ${AWS_ACCESS_KEY_ID:-}
YAML
refused live-credentials "a sealed value may not fill services.migrate.environment.AWS_ACCESS_KEY_ID" \
    --images "$tmp/images.json" "${origin[@]}" "$tmp/live-credentials"

# The template variant: the service without dstack-ingress, topup on port 80 for the gateway, and
# only the deploy form's values at runtime (deploy/compose.template.yaml).
template="$root/deploy/environments/phala-cloud-template/topup"
render --template "$template" >"$tmp/template.yml"
[[ "$("$compose" -f "$tmp/template.yml" config --variables | awk 'NR > 1 { print $1 }' | sort |
    tr '\n' ' ')" == "AWS_ACCESS_KEY_ID AWS_ENDPOINT AWS_REGION AWS_SECRET_ACCESS_KEY DSTACK_APP_DOMAIN SENTRY_DSN TOPUP_ADMIN_PUBLIC_KEY WALG_S3_PREFIX " ]] ||
    { echo "the template's runtime names changed" >&2; exit 1; }
refused template-gateway "--template serves the app's gateway domain" --images "$tmp/images.json" \
    --template "${gateway[@]}" "$template"
refused template-restore-check "usage:" --images "$tmp/images.json" --template "${origin[@]}" "$template"
refused template-as-service "a sealed value may not fill services.postgres.environment.WALG_S3_PREFIX" \
    --images "$tmp/images.json" "${gateway[@]}" "$template"
refused service-as-template "the template's public_origin must be the app's gateway domain" \
    --images "$tmp/images.json" --template "$staging"
# A runtime value fills only the form's settings: not another line of topup.yaml, not another
# service's environment; and topup publishes only port 80.
cp -r "$template" "$tmp/template-key-id"
sed -i 's|id: admin/v1|id: ${TOPUP_ADMIN_PUBLIC_KEY:-}|' "$tmp/template-key-id/topup.yaml"
refused template-key-id "a sealed value may not fill configs.topup_" --images "$tmp/images.json" \
    --template "$tmp/template-key-id"
cp -r "$template" "$tmp/template-rpc"
sed -i 's|^  provider-a: .*|  provider-a: https://${DSTACK_APP_DOMAIN:-}|' "$tmp/template-rpc/topup.yaml"
refused template-rpc "a sealed value may not fill configs.topup_" --images "$tmp/images.json" \
    --template "$tmp/template-rpc"
cp -r "$template" "$tmp/template-topup-env"
cat >>"$tmp/template-topup-env/compose.yaml" <<'YAML'
  topup:
    environment:
      WALG_S3_PREFIX: ${WALG_S3_PREFIX:-}
YAML
refused template-topup-env "a sealed value may not fill services.topup.environment.WALG_S3_PREFIX" \
    --images "$tmp/images.json" --template "$tmp/template-topup-env"
cp -r "$template" "$tmp/template-port"
cat >>"$tmp/template-port/compose.yaml" <<'YAML'
  migrate:
    ports: ["5432:5432"]
YAML
refused template-port "only topup may publish a port, 80" --images "$tmp/images.json" --template \
    "$tmp/template-port"

# The product: its one sealed name, and its public_url on its domain.
product="$root/deploy/environments/phala-network/staging/product"
render "${gateway[@]}" "$product" >"$tmp/product.yml"
[[ "$("$compose" -f "$tmp/product.yml" config --variables | awk 'NR > 1 { print $1 }')" == PRODUCT_API_KEY ]]
cp -r "$product" "$tmp/product-url"
jq '.public_url = "https://other.phala.com"' "$product/config.json" >"$tmp/product-url/config.json"
refused product-url "dstack-ingress must serve the host of the product's public_url" \
    --images "$tmp/images.json" "${gateway[@]}" "$tmp/product-url"

if grep -rqF 'staging-v2' "$tmp"/*.err; then
    echo "render.sh printed a configuration value" >&2
    exit 1
fi
echo "compose renderer test passed"
