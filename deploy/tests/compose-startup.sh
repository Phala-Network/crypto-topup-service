#!/usr/bin/env bash
# Exercise Docker's inline-config injection on real render.sh output with the CVM's Compose.
# Replace application processes with a bounded shell probe, removing external dependencies,
# mounts and ports. Keep configs, users, rootfs policy, tmpfs and all security/resource options.
# This tests container creation/recreation and config permissions, not application health.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-compose-startup.XXXXXX")
project="topup-compose-startup-$$"
probe=alpine:3.22@sha256:5291449c3df73caf6ed85e649dec1b9e818b39a5d8c871e97afc13e9cd5e8fa8
cleanup() {
    for file in "$tmp"/probe-*.json; do
        [[ -f "$file" ]] || continue
        "$compose" -f "$file" down --timeout 1 >/dev/null 2>&1 || true
    done
    rm -rf "$tmp"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
compose=$("$root/deploy/pinned-compose.sh")
docker image inspect "$probe" >/dev/null 2>&1 || docker pull "$probe" >/dev/null
jq -n --arg image "$probe" '{"phala-pay": $image, "postgres-walg": $image,
    "phala-pay-reference-product": $image}' >"$tmp/images.json"
staging="$root/deploy/environments/phala-network/staging"
# Product's user is supplied by its image, rather than Compose; preserve it in the shell probe.
product_user=$(awk '$1 == "USER" { user = $2 } END { print user }' "$root/deploy/Dockerfile.reference-product")
[[ "$product_user" =~ ^[1-9][0-9]*:[1-9][0-9]*$ ]]

for variant in service restore-check template product; do
    inputs=(--gateway-domain gateway.dstack-pha-prod5.phala.network)
    environment="$staging/topup"
    case "$variant" in
        restore-check) inputs=(--restore-check --origin https://restore.example.net) ;;
        template) inputs=(--template); environment="$root/deploy/environments/phala-cloud-template/topup" ;;
        product) environment="$staging/product" ;;
    esac
    "$root/deploy/render.sh" "${inputs[@]}" --project-name "$project-$variant" \
        --images "$tmp/images.json" "$environment" >"$tmp/$variant.yml"
    "$compose" -f "$tmp/$variant.yml" config --no-interpolate --format json |
        jq --arg image "$probe" --arg product_user "$product_user" '
            .services |= with_entries(select((.value.configs // [] | length > 0)
                or (.key | IN("heartbeat", "smokescreen"))) | .value |= (
                del(.depends_on, .volumes, .environment, .ports, .healthcheck, .networks)
                | .image = $image | .entrypoint = ["/bin/sh", "-c"]
                | .command = ["sleep 300"] | .restart = "no" | .stop_grace_period = "1s"
                | .network_mode = "none"))
            | del(.volumes, .networks)
            | if .services.product then .services.product.user = $product_user else . end
        ' >"$tmp/probe-$variant.json"
    file="$tmp/probe-$variant.json"
    # config output re-escapes dollars for another Compose load; up injects single dollars.
    "$compose" -f "$file" config --format json >"$tmp/expected.json"
    for pass in create recreate; do
        "$compose" -f "$file" up -d --force-recreate --pull never >/dev/null
        while IFS=$'\t' read -r service source target; do
            id=$("$compose" -f "$file" ps -q "$service")
            jq -j --arg source "$source" '.configs[$source].content | gsub("\\$\\$"; "$")' "$tmp/expected.json" \
                >"$tmp/expected-content"
            docker exec "$id" cat "$target" >"$tmp/actual-content"
            cmp "$tmp/expected-content" "$tmp/actual-content"
            if [[ "$service" == topup || "$service" == product ]]; then
                [[ "$(docker exec "$id" stat -c '%u:%g:%a' "$target")" == 0:0:444 ]]
                docker exec "$id" sh -c 'test "$(id -u)" != 0 && test ! -w "$1" && test ! -w "$(dirname "$1")"' sh "$target"
            fi
        done < <(jq -r '.services | to_entries[] | .key as $service | .value.configs[]?
            | [$service, .source, .target] | @tsv' "$tmp/expected.json")
        for service in heartbeat smokescreen; do
            if jq -e --arg service "$service" '.services[$service] != null' "$file" >/dev/null; then
                id=$("$compose" -f "$file" ps -q "$service")
                docker exec --user 0 "$id" sh -c 'if touch /etc/startup-probe 2>/dev/null; then exit 1; fi'
            fi
        done
        echo "ok: $variant $pass, config bytes/permissions and read-only sidecars"
    done
    "$compose" -f "$file" down --timeout 1 >/dev/null
done

# Negative control: reproduce the release error even with tmpfs at the config's parent.
jq --arg project "$project-negative" '.name = $project
    | .services = {topup: .services.topup}
    | .services.topup.read_only = true
    | .services.topup.tmpfs += ["/etc/topup"]' "$tmp/probe-service.json" >"$tmp/probe-negative.json"
if "$compose" -f "$tmp/probe-negative.json" up -d --pull never >"$tmp/negative.log" 2>&1; then
    echo "read-only inline-config negative control unexpectedly started" >&2
    exit 1
fi
if ! grep -F 'container rootfs is marked read-only' "$tmp/negative.log" >/dev/null; then
    cat "$tmp/negative.log" >&2
    exit 1
fi
echo "ok: read-only inline config reproduces the Docker error, including with target tmpfs"
