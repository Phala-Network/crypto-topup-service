#!/usr/bin/env bash
# Preflight for a staging deploy (deploy/README.md, "First staging deploy checklist"). It is
# read-only against remote systems: it never pushes, deploys, updates, or sends a transaction. It
# reads the env file (the owner-sealed secrets), the rendered compose (which holds the public
# settings, deploy/README.md "Attested settings"), the images in their registry, the asset chain
# through the compose's two RPC providers, and the Phala Cloud account the CLI is logged in to.
# Locally it renders the compose, builds the contracts (verify-deployment.sh), and pulls both
# images.
#
# Usage: deploy/preflight.sh --env .env.staging --compose deploy/docker-compose.staging.yml \
#          --workspace NAME --os-image NAME [--source COMPOSE] [--restore-check] [--offline] \
#          [--unsealed]
#
# --os-image must be the owner-approved OS image, dstack-0.5.9 (deploy/README.md): the pinned dstack
# SDK speaks the dstack 0.5 guest API. Online, the image must be a listed production image and a
# node of the workspace must offer it (`api /teepods/available`), or provisioning fails with
# "OS image ... is not available on the selected node".
# Every CVM uses Phala Cloud's KMS (`--kms phala`), so there is no KMS contract to check. Images
# are pulled anonymously (an empty Docker client config), because the CVM pulls them without
# credentials: a private image fails here.
#
# --source is the unrendered compose the rendered file must come from (default
# deploy/docker-compose.yml of this checkout). --restore-check expects the compose rendered with
# render-compose.sh --restore-check (deploy/RESTORE.md). --offline runs only the local checks (env
# file, compose, route). --unsealed accepts empty owner-sealed secrets: Deploy provisions
# with them empty and the owner seals them from their own machine; check that file without
# --unsealed. PHALA selects the CLI command (default `npx --yes phala@1.1.22`). Every failure is
# reported; the exit status is 1 if any.
#
# The RPC URLs are attested and published with the compose, so they must be keyless public URLs.
# Output still never prints them: tool errors are redacted to "provider a" and "provider b".
set -euo pipefail
source "$(dirname -- "$0")/contracts/common.sh"
source "$(dirname -- "$0")/preflight-phala.sh"

root="$REPO_ROOT"
example="$root/deploy/staging.env.example"
expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
route_config=topup_route_phala_cloud_sepolia_pha
# May stay empty: an empty DSN turns Sentry reporting off.
optional_empty=" SENTRY_DSN "
# The owner-approved OS image (deploy/README.md, "OS image"): production, dstack 0.5.9.
approved_os_image=dstack-0.5.9

usage() {
    echo "usage: $0 --env FILE --compose FILE --workspace NAME --os-image NAME" \
        "[--source COMPOSE] [--restore-check]" \
        "[--offline] [--unsealed]" >&2
    exit 64
}
env_file="" compose="" workspace="" os_image="" offline=0 unsealed=0 variant=()
source_compose="$REPO_ROOT/deploy/docker-compose.yml"
while (($#)); do
    case "$1" in
        --env) env_file="${2:-}"; shift 2 ;;
        --compose) compose="${2:-}"; shift 2 ;;
        --workspace) workspace="${2:-}"; shift 2 ;;
        --os-image) os_image="${2:-}"; shift 2 ;;
        --source) source_compose="${2:-}"; shift 2 ;;
        --restore-check) variant=(--restore-check); shift ;;
        --offline) offline=1; shift ;;
        --unsealed) unsealed=1; shift ;;
        *) usage ;;
    esac
done
[[ -f "$env_file" && -f "$compose" ]] || usage
((offline)) || [[ -n "$workspace" && -n "$os_image" ]] || usage
for command in docker jq; do
    require_command "$command"
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-preflight.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
failures=0
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}
ok() {
    printf 'ok: %s\n' "$*"
}

# Placeholder addresses: zero, or one repeated hex digit (the template uses 0x1111..., 0x2222...).
placeholder_address() {
    grep -Eiq '^0x([0-9a-f])\1{39}$' <<<"$1"
}

echo "== env file"
declare -A env=()
names_of() {
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$1"
}
if grep -Evq '^([[:space:]]*($|#)|[A-Za-z_][A-Za-z0-9_]*=)' "$env_file"; then
    fail "$env_file has a line that is not KEY=VALUE"
fi
duplicates=$(names_of "$env_file" | sort | uniq -d)
[[ -z "$duplicates" ]] || fail "$env_file sets a name twice: $(tr '\n' ' ' <<<"$duplicates")"
names_of "$example" | sort -u >"$tmp/expected"
names_of "$env_file" | sort -u >"$tmp/actual"
missing=$(comm -23 "$tmp/expected" "$tmp/actual")
extra=$(comm -13 "$tmp/expected" "$tmp/actual")
[[ -z "$missing" ]] || fail "$env_file is missing: $(tr '\n' ' ' <<<"$missing")"
# An extra name changes the CLI's allowed_envs and therefore the compose hash.
[[ -z "$extra" ]] || fail "$env_file has names outside staging.env.example: $(tr '\n' ' ' <<<"$extra")"
while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*($|#) ]] && continue
    env[${line%%=*}]=${line#*=}
done <"$env_file"
while IFS= read -r name; do
    value=${env[$name]-}
    if [[ "$value" == *replace-me* ]]; then
        fail "$name still contains replace-me"
    elif [[ -z "$value" ]] && ((unsealed == 0)) && [[ "$optional_empty" != *" $name "* ]]; then
        fail "$name is empty"
    fi
done <"$tmp/expected"
# Empty turns Sentry reporting off; the service refuses to start with a malformed DSN.
sentry_dsn=${env[SENTRY_DSN]-}
[[ -z "$sentry_dsn" || "$sentry_dsn" =~ ^https://[0-9a-f]{32}@[a-z0-9.-]+/[0-9]+$ ]] ||
    fail "SENTRY_DSN must be empty or the project's DSN, https://KEY@HOST/PROJECT_ID"

if [[ -n "$os_image" && "$os_image" != "$approved_os_image" ]]; then
    fail "OS image $os_image is not the approved $approved_os_image (deploy/README.md)"
fi

echo "== compose"
if docker compose -f "$compose" config --no-interpolate --format json >"$tmp/compose.json" \
    2>"$tmp/compose.err"; then
    jq -r '.services[] | .image' "$tmp/compose.json" | sort -u >"$tmp/images"
    while IFS= read -r image; do
        if ! [[ "$image" =~ ^[^@]+@sha256:[0-9a-f]{64}$ ]] || [[ "$image" == *@sha256:0000000000000000000000000000000000000000000000000000000000000000 ]]; then
            fail "image $image is not a nonzero repository@sha256 digest; run render-compose.sh"
        fi
    done <"$tmp/images"
    # The CLI derives allowed_envs from the env file, so it must name exactly what the compose reads.
    docker compose -f "$compose" config --variables 2>/dev/null |
        awk 'NR > 1 && NF > 0 { print $1 }' | sort >"$tmp/compose-variables"
    cmp -s "$tmp/compose-variables" "$tmp/expected" ||
        fail "the compose reads other variables than staging.env.example:" \
            "$(diff "$tmp/expected" "$tmp/compose-variables" | grep '^[<>]' | tr '\n' ' ')"
    jq -j --arg name "$route_config" '.configs[$name].content // empty' "$tmp/compose.json" \
        >"$tmp/route.yaml"
    # The public settings, from the attested compose.
    declare -A setting=()
    while IFS=$'\t' read -r name value; do
        setting[$name]=$value
    done < <(jq -r '.services as $s | ($s.topup.environment + $s.postgres.environment) as $e
        | ((["TOPUP_ADMIN_KID", "TOPUP_ADMIN_PUBLIC_KEY", "TOPUP_PUBLIC_ORIGIN",
            "TOPUP_RPC_PROVIDER_A_URL", "TOPUP_RPC_PROVIDER_B_URL", "TOPUP_SERVICE_ENABLED",
            "WALG_S3_PREFIX", "AWS_ENDPOINT", "AWS_REGION", "AWS_S3_FORCE_PATH_STYLE",
            "TOPUP_RESTORE_FROM_BACKUP"][]
            | [., ($e[.] // "" | strings)]),
          (($s["dstack-ingress"].environment // {}) as $i | ["DOMAIN", "GATEWAY_DOMAIN"][]
            | ["INGRESS_\(.)", ($i[.] // "" | strings)]))
        | @tsv' \
        "$tmp/compose.json")
    # TOPUP_PUBLIC_ORIGIN is https://TOPUP_DOMAIN: the custom domain dstack-ingress serves, or the
    # restore-check instance's gateway host.
    hostname='^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$'
    origin=${setting[TOPUP_PUBLIC_ORIGIN]-}
    setting[TOPUP_DOMAIN]=${origin#https://}
    if [[ "$origin" == https://* && "${setting[TOPUP_DOMAIN]}" =~ $hostname ]]; then
        if [[ "$origin" == *.invalid ]]; then
            echo "note: TOPUP_DOMAIN is provisional; the restore-check instance's gateway host" \
                "replaces it (deploy/RESTORE.md)"
        fi
    else
        fail "TOPUP_DOMAIN must be a lowercase host name (TOPUP_PUBLIC_ORIGIN https://TOPUP_DOMAIN)"
    fi
    setting[TOPUP_GATEWAY_DOMAIN]=${setting[INGRESS_GATEWAY_DOMAIN]-}
    if ((${#variant[@]} == 0)); then
        [[ "${setting[INGRESS_DOMAIN]-}" == "${setting[TOPUP_DOMAIN]}" ]] ||
            fail "dstack-ingress must serve TOPUP_DOMAIN, the host of TOPUP_PUBLIC_ORIGIN"
        [[ "${setting[TOPUP_GATEWAY_DOMAIN]}" =~ $hostname ]] ||
            fail "TOPUP_GATEWAY_DOMAIN must be the dstack gateway's host name, for example" \
                "gateway.dstack-pha-prod5.phala.network"
    fi
    unset 'setting[INGRESS_DOMAIN]' 'setting[INGRESS_GATEWAY_DOMAIN]' 'setting[TOPUP_PUBLIC_ORIGIN]'
    rpc_a=${setting[TOPUP_RPC_PROVIDER_A_URL]-} rpc_b=${setting[TOPUP_RPC_PROVIDER_B_URL]-}
    [[ "$rpc_a" == https://* && "$rpc_b" == https://* ]] ||
        fail "both RPC provider URLs must use https"
    [[ "$rpc_a" != "$rpc_b" ]] || fail "the two RPC provider URLs must be different providers"
    admin_key_bytes=$(base64 -d 2>/dev/null <<<"${setting[TOPUP_ADMIN_PUBLIC_KEY]-}" | wc -c) ||
        admin_key_bytes=0
    [[ "$admin_key_bytes" == 32 ]] || fail "TOPUP_ADMIN_PUBLIC_KEY must be standard base64 of 32 bytes"
    [[ "${setting[WALG_S3_PREFIX]-}" == s3://?* ]] || fail "WALG_S3_PREFIX must be s3://BUCKET/PATH"
    [[ "${setting[AWS_ENDPOINT]-}" == https://?* ]] || fail "AWS_ENDPOINT must be an https:// URL"
    [[ "${setting[AWS_S3_FORCE_PATH_STYLE]-}" =~ ^(true|false)$ ]] ||
        fail "AWS_S3_FORCE_PATH_STYLE must be true or false"
    if ((${#variant[@]})); then
        expected_modes="on read-only"
    else
        expected_modes="off on"
    fi
    [[ "${setting[TOPUP_RESTORE_FROM_BACKUP]-} ${setting[TOPUP_SERVICE_ENABLED]-}" == "$expected_modes" ]] ||
        fail "the compose is not the ${variant[*]:-service} variant (TOPUP_RESTORE_FROM_BACKUP" \
            "${setting[TOPUP_RESTORE_FROM_BACKUP]-}, TOPUP_SERVICE_ENABLED ${setting[TOPUP_SERVICE_ENABLED]-})"
    # A stale render (older checkout, hand edits) must not reach the CLI: re-render the source
    # with the same images and settings and compare byte for byte.
    render_env=()
    for name in "${!setting[@]}"; do
        render_env+=("$name=${setting[$name]}")
    done
    if env "${render_env[@]}" TOPUP_IMAGE="$(jq -r '.services.topup.image' "$tmp/compose.json")" \
        POSTGRES_WALG_IMAGE="$(jq -r '.services.postgres.image' "$tmp/compose.json")" \
        "$root/deploy/render-compose.sh" "${variant[@]}" "$source_compose" >"$tmp/fresh.yml" 2>/dev/null &&
        cmp -s "$tmp/fresh.yml" "$compose"; then
        :
    else
        fail "$compose differs from a fresh render of $source_compose with the same images and" \
            "settings; re-run render-compose.sh from the commit being deployed"
    fi
else
    fail "docker compose cannot parse $compose: $(head -c 300 "$tmp/compose.err")"
    : >"$tmp/route.yaml"
    : >"$tmp/images"
fi

echo "== route"
route_value() {
    sed -n "s/^[[:space:]]*$1:[[:space:]]*\"\{0,1\}\([^\"#[:space:]]*\)\"\{0,1\}.*/\1/p" \
        "$tmp/route.yaml" | head -n 1
}
declare -A route=()
if [[ -s "$tmp/route.yaml" ]]; then
    for key in chain_id forwarder_factory implementation treasury contract sanctions_oracle \
        decimals; do
        route[$key]=$(route_value "$key")
    done
    for key in forwarder_factory implementation treasury contract sanctions_oracle; do
        address=${route[$key]}
        if ! is_address "$address"; then
            fail "route $key is not an address: '$address'"
        elif placeholder_address "$address"; then
            fail "route $key is the placeholder or zero address $address; deploy the contracts" \
                "(deploy/CONTRACTS.md) and commit the real address"
        fi
    done
else
    fail "the compose has no inline $route_config config"
fi

if ((failures)); then
    echo "preflight: $failures local check(s) failed; online checks not run" >&2
    exit 1
fi
if ((offline)); then
    echo "preflight: local checks passed (offline)"
    exit 0
fi

check_anonymous_pulls "$tmp/images"
topup_image=$(jq -r '.services.topup.image' "$tmp/compose.json")
if docker run --rm -i --pull never "$topup_image" topup route validate /dev/stdin \
    <"$tmp/route.yaml" \
    >"$tmp/validate.out" 2>&1; then
    ok "topup route validate accepts the attested route"
else
    fail "topup route validate rejected the route: $(tail -n 3 "$tmp/validate.out")"
fi

echo "== asset chain (RPC URLs are not printed)"
for command in cast forge; do
    require_command "$command"
done
redact() {
    local text=$1
    text=${text//"$rpc_a"/provider a}
    printf '%s' "${text//"$rpc_b"/provider b}"
}
chain_ok=1
for label in a b; do
    [[ "$label" == a ]] && url=$rpc_a || url=$rpc_b
    id=$(ETH_RPC_URL=$url cast chain-id 2>/dev/null) || id=error
    if [[ "$id" == "${route[chain_id]}" ]]; then
        ok "provider $label reports chain id $id"
    else
        fail "provider $label reports chain id $id, the route needs ${route[chain_id]}"
        chain_ok=0
    fi
done
network=$(jq -r --argjson id "${route[chain_id]}" \
    '.networks | to_entries[] | select(.value.chain_id == $id) | .key' "$expectations")
if ((chain_ok)) && [[ -n "$network" ]]; then
    if ADMIN=$(jq -r .admin "$expectations") TREASURY=$(jq -r .treasury "$expectations") \
        "$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" --rpc "$network/a=$rpc_a" \
        --rpc "$network/b=$rpc_b" >"$tmp/verification.json" 2>"$tmp/verification.err"; then
        ok "verify-deployment.sh passed on both providers"
    else
        fail "verify-deployment.sh failed: $(redact "$(tail -n 3 "$tmp/verification.err")")"
    fi
    if jq -e --arg factory "${route[forwarder_factory]}" \
        --arg implementation "${route[implementation]}" --arg treasury "${route[treasury]}" \
        '(.chains | length) == 2 and all(.chains[];
            (.factory | ascii_downcase) == ($factory | ascii_downcase) and
            (.implementation | ascii_downcase) == ($implementation | ascii_downcase) and
            (.treasury | ascii_downcase) == ($treasury | ascii_downcase))' \
        "$tmp/verification.json" >/dev/null 2>&1; then
        ok "route factory, implementation, and treasury match the verified deployment"
    else
        fail "route contract addresses differ from the verified deployment"
    fi
    for label in a b; do
        [[ "$label" == a ]] && url=$rpc_a || url=$rpc_b
        for key in contract sanctions_oracle; do
            code=$(ETH_RPC_URL=$url cast code "${route[$key]}" 2>/dev/null) || code=error
            [[ "$code" =~ ^0x[0-9a-fA-F]+$ && "$code" != 0x ]] ||
                fail "route $key ${route[$key]} has no code on provider $label"
        done
    done
    decimals=$(ETH_RPC_URL=$rpc_a cast call "${route[contract]}" 'decimals()(uint8)' 2>/dev/null) ||
        decimals=error
    [[ "$decimals" == "${route[decimals]}" ]] ||
        fail "asset decimals() is $decimals, the route says ${route[decimals]}"
elif ((chain_ok)); then
    fail "$expectations names no network with chain id ${route[chain_id]}"
fi

check_phala_cloud "$workspace" "$os_image"

if ((failures)); then
    echo "preflight: $failures check(s) failed" >&2
    exit 1
fi
echo "preflight: all checks passed"
