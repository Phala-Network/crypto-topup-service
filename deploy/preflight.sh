#!/usr/bin/env bash
# Preflight for a staging deploy (deploy/README.md, "First staging deploy checklist"). It is
# read-only against remote systems: it never pushes, deploys, updates, or sends a transaction. It
# reads the env file, the rendered compose, the images in their registry, the asset chain through
# the env file's two RPC providers, and the Phala Cloud account the CLI is logged in to. Locally
# it renders the compose, builds the contracts (verify-deployment.sh), and pulls both images.
#
# Usage: deploy/preflight.sh --env .env.staging --compose deploy/docker-compose.staging.yml \
#          --workspace NAME --os-image NAME [--kms base|phala] [--kms-contract ADDRESS] \
#          [--source COMPOSE] [--offline]
#
# --kms base (default) also checks that the on-chain KMS contract allows a device and the OS image;
# --kms phala (Phala Cloud's KMS, used for staging) has no contract to check. Images are pulled
# anonymously (an empty Docker client config), because the CVM pulls them without credentials: a
# private image fails here.
#
# --source is the unrendered compose the rendered file must come from (default
# deploy/docker-compose.yml of this checkout). --offline runs only the local checks (env file,
# compose, route). PHALA selects the CLI command (default `npx --yes phala@1.1.22`). Every failure
# is reported; the exit status is 1 if any.
#
# RPC URLs may carry provider API keys. Cast reads them from ETH_RPC_URL here, but
# verify-deployment.sh takes them as arguments, so they are visible in the process list while it
# runs; run preflight on a single-user machine. Output never prints them: tool errors are
# redacted to "provider a" and "provider b".
set -euo pipefail
source "$(dirname -- "$0")/contracts/common.sh"

root="$REPO_ROOT"
example="$root/deploy/staging.env.example"
expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
route_config=topup_route_phala_cloud_sepolia_pha
# May stay empty: a static S3 key has no session token, AWS S3 needs no endpoint, and an empty
# Coin Metrics key selects the community endpoint.
optional_empty=" AWS_SESSION_TOKEN AWS_ENDPOINT COINMETRICS_API_KEY "

usage() {
    echo "usage: $0 --env FILE --compose FILE --workspace NAME --os-image NAME" \
        "[--kms base|phala] [--kms-contract ADDRESS] [--source COMPOSE] [--offline]" >&2
    exit 64
}
env_file="" compose="" workspace="" os_image="" kms=base offline=0
source_compose="$REPO_ROOT/deploy/docker-compose.yml"
kms_contract=0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C
while (($#)); do
    case "$1" in
        --env) env_file="${2:-}"; shift 2 ;;
        --compose) compose="${2:-}"; shift 2 ;;
        --workspace) workspace="${2:-}"; shift 2 ;;
        --os-image) os_image="${2:-}"; shift 2 ;;
        --kms) kms="${2:-}"; shift 2 ;;
        --kms-contract) kms_contract="${2:-}"; shift 2 ;;
        --source) source_compose="${2:-}"; shift 2 ;;
        --offline) offline=1; shift ;;
        *) usage ;;
    esac
done
[[ -f "$env_file" && -f "$compose" ]] || usage
((offline)) || [[ -n "$workspace" && -n "$os_image" ]] || usage
[[ "$kms" == base || "$kms" == phala ]] || usage
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
for name in $(cat "$tmp/expected"); do
    value=${env[$name]-}
    if [[ "$value" == *replace-me* ]]; then
        fail "$name still contains replace-me"
    elif [[ -z "$value" && "$optional_empty" != *" $name "* ]]; then
        fail "$name is empty"
    fi
done
origin=${env[TOPUP_PUBLIC_ORIGIN]-}
if [[ "$origin" =~ ^https://[a-z0-9.-]+(:[0-9]+)?$ ]]; then
    if [[ "$origin" == *.invalid || "$origin" == *.invalid:* ]]; then
        echo "note: TOPUP_PUBLIC_ORIGIN is provisional; replace it with the gateway URL after" \
            "provisioning (deploy/README.md) before issuing product credentials"
    fi
else
    fail "TOPUP_PUBLIC_ORIGIN must be https://HOST[:PORT] in lowercase with no path"
fi
rpc_a=${env[TOPUP_RPC_PROVIDER_A_URL]-} rpc_b=${env[TOPUP_RPC_PROVIDER_B_URL]-}
[[ "$rpc_a" == https://* && "$rpc_b" == https://* ]] ||
    fail "both RPC provider URLs must use https"
[[ "$rpc_a" != "$rpc_b" ]] || fail "the two RPC provider URLs must be different providers"
admin_key_bytes=$(base64 -d 2>/dev/null <<<"${env[TOPUP_ADMIN_PUBLIC_KEY]-}" | wc -c) || admin_key_bytes=0
[[ "$admin_key_bytes" == 32 ]] || fail "TOPUP_ADMIN_PUBLIC_KEY must be standard base64 of 32 bytes"
[[ "${env[WALG_S3_PREFIX]-}" == s3://?* ]] || fail "WALG_S3_PREFIX must be s3://BUCKET/PATH"
[[ "${env[TOPUP_WAL_ARCHIVE]-}" == on ]] || fail "TOPUP_WAL_ARCHIVE must be on for staging"
[[ "${env[TOPUP_SERVICE_ENABLED]-}" == on ]] || fail "TOPUP_SERVICE_ENABLED must be on for staging"

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
    # A stale render (older checkout, hand edits) must not reach the CLI: re-render the source
    # with the same image references and compare byte for byte.
    if TOPUP_IMAGE=$(jq -r '.services.topup.image' "$tmp/compose.json") \
        POSTGRES_WALG_IMAGE=$(jq -r '.services.postgres.image' "$tmp/compose.json") \
        "$root/deploy/render-compose.sh" "$source_compose" >"$tmp/fresh.yml" 2>/dev/null &&
        cmp -s "$tmp/fresh.yml" "$compose"; then
        :
    else
        fail "$compose differs from a fresh render of $source_compose with the same images;" \
            "re-run render-compose.sh from the commit being deployed"
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
        decimals settlement_url; do
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
    [[ "${route[settlement_url]}" == https://* ]] || fail "route settlement_url must use https"
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

echo "== images (anonymous pull)"
# `docker pull` of a digest always asks the registry, even when the daemon has the image cached;
# an empty client config sends no credentials, as the CVM does.
# The empty config also drops the current Docker context, so keep its daemon endpoint.
docker_host=${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null)} ||
    docker_host=""
mkdir "$tmp/docker-anonymous"
while IFS= read -r image; do
    if DOCKER_HOST=${docker_host:-unix:///var/run/docker.sock} DOCKER_CONFIG="$tmp/docker-anonymous" \
        docker pull --quiet --platform linux/amd64 "$image" >/dev/null 2>&1; then
        ok "$image pulls anonymously"
    else
        fail "$image cannot be pulled anonymously; make the package public"
    fi
done <"$tmp/images"
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

echo "== Phala Cloud (read-only)"
read -r -a phala <<<"${PHALA:-npx --yes phala@1.1.22}"
version=$("${phala[@]}" --version 2>/dev/null) || version=error
[[ "$version" == v1.1.22* || "$version" == 1.1.22* ]] ||
    fail "the Phala CLI is $version; these steps are verified against 1.1.22"
# Best effort: `status --json` in CLI 1.1.22 reports only the workspace display name (team_name),
# no workspace id, so two workspaces with the same name cannot be told apart here.
if "${phala[@]}" status --json >"$tmp/status.json" 2>/dev/null &&
    jq -e --arg workspace "$workspace" '.team_name == $workspace' "$tmp/status.json" >/dev/null; then
    ok "logged in to a workspace named $workspace (display name; best effort)"
else
    current=$(jq -r '.team_name // empty' "$tmp/status.json" 2>/dev/null) || current=""
    fail "the CLI is not logged in to workspace '$workspace' (current: ${current:-not logged in})"
fi
if [[ "$kms" == base ]]; then
    if "${phala[@]}" kms base --json >"$tmp/kms.json" 2>/dev/null; then
        contract=$(jq -c --arg address "$(lower "$kms_contract")" \
            '[.contracts[] | select((.contract_address | ascii_downcase) == $address)][0] // empty' \
            "$tmp/kms.json")
        if [[ -z "$contract" ]]; then
            fail "KMS contract $kms_contract is not listed by 'kms base'"
        else
            jq -e '[.devices[] | select(.on_chain_allowed == true)] | length > 0' <<<"$contract" \
                >/dev/null || fail "KMS contract $kms_contract has no allowed device"
            if jq -e --arg image "$os_image" \
                'any(.os_images[]; .name == $image and .on_chain_allowed == true)' <<<"$contract" \
                >/dev/null; then
                ok "OS image $os_image is allowed by KMS contract $kms_contract"
            else
                fail "OS image $os_image is not allowed by KMS contract $kms_contract; allowed:" \
                    "$(jq -r '[.os_images[] | select(.on_chain_allowed == true) | .name] | join(", ")' \
                        <<<"$contract")"
            fi
        fi
    else
        fail "'kms base --json' failed"
    fi
fi
if "${phala[@]}" os-images --prod --all --json >"$tmp/os-images.json" 2>/dev/null &&
    jq -e --arg image "$os_image" 'any(.items[]; .name == $image and .is_dev == false)' \
        "$tmp/os-images.json" >/dev/null; then
    ok "OS image $os_image is a production (non-dev) image"
    # The pinned dstack SDK needs the /v1 guest API of dstack 0.6 (deploy/README.md).
    jq -e --arg image "$os_image" \
        'any(.items[]; .name == $image and (.version | test("^v?0[.]6[.]")))' \
        "$tmp/os-images.json" >/dev/null || fail "OS image $os_image is not a dstack 0.6 image"
else
    fail "OS image $os_image is not listed as a production image by 'os-images --prod'"
fi

if ((failures)); then
    echo "preflight: $failures check(s) failed" >&2
    exit 1
fi
echo "preflight: all checks passed"
