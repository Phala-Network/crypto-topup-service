#!/usr/bin/env bash
# Preflight for the staging reference-product CVM (deploy/README.md, "Staging reference product").
# Read-only against remote systems, like deploy/preflight.sh: it reads the env file, the compose
# rendered by deploy/product/render-compose.sh (which holds the public settings), the image in its
# registry, the product's RPC, topup's RPC providers, topup's attestation endpoint, and the Phala
# Cloud account the CLI is logged in to.
#
# Usage: deploy/product/preflight.sh --env FILE --compose FILE --workspace NAME --os-image NAME
#          [--offline] [--unsealed]
#
# --offline runs only the local checks (env file and compose). --unsealed accepts an empty
# PRODUCT_SEED: Deploy staging product provisions without it and the owner seals it from their own
# machine. The online checks compare the product RPC's finalized block with topup's providers,
# TOPUP_RPC_PROVIDER_A_URL and TOPUP_RPC_PROVIDER_B_URL from the environment. Output never prints
# an RPC URL. Every failure is reported; the exit status is 1 if any.
set -euo pipefail
source "$(dirname -- "$0")/../contracts/common.sh"
source "$(dirname -- "$0")/../preflight-phala.sh"

root="$REPO_ROOT"
example="$root/deploy/product/staging.env.example"
source_compose="$root/deploy/product/docker-compose.yml"
approved_os_image=dstack-0.5.9
# The product answers a settlement 503 until its RPC has finalized the deposit's block, so an RPC
# whose finalized block trails topup's providers by more than this stalls every settlement.
max_finalized_lag=64

usage() {
    echo "usage: $0 --env FILE --compose FILE --workspace NAME --os-image NAME [--offline] [--unsealed]" >&2
    exit 64
}
env_file="" compose="" workspace="" os_image="" offline=0 unsealed=0
while (($#)); do
    case "$1" in
        --env) env_file="${2:-}"; shift 2 ;;
        --compose) compose="${2:-}"; shift 2 ;;
        --workspace) workspace="${2:-}"; shift 2 ;;
        --os-image) os_image="${2:-}"; shift 2 ;;
        --offline) offline=1; shift ;;
        --unsealed) unsealed=1; shift ;;
        *) usage ;;
    esac
done
[[ -f "$env_file" && -f "$compose" ]] || usage
((offline)) || [[ -n "$workspace" && -n "$os_image" ]] || usage
for command in docker jq curl; do
    require_command "$command"
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/product-preflight.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
failures=0
fail() {
    printf 'FAIL: %s\n' "$*" >&2
    failures=$((failures + 1))
}
ok() {
    printf 'ok: %s\n' "$*"
}
names_of() {
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$1"
}
origin_pattern='^https://[a-z0-9.-]+(:[0-9]+)?$'

echo "== env file"
declare -A env=()
if grep -Evq '^([[:space:]]*($|#)|[A-Za-z_][A-Za-z0-9_]*=)' "$env_file"; then
    fail "$env_file has a line that is not KEY=VALUE"
fi
names_of "$example" | sort -u >"$tmp/expected"
names_of "$env_file" | sort >"$tmp/actual"
# Extra, missing, or repeated names change the CLI's allowed_envs and so the compose hash.
cmp -s "$tmp/expected" "$tmp/actual" ||
    fail "$env_file must set exactly the names of $example once each:" \
        "$(diff "$tmp/expected" "$tmp/actual" | grep '^[<>]' | tr '\n' ' ')"
while IFS= read -r line; do
    [[ "$line" =~ ^[[:space:]]*($|#) ]] && continue
    env[${line%%=*}]=${line#*=}
done <"$env_file"
for name in $(cat "$tmp/expected"); do
    value=${env[$name]-}
    if [[ "$value" == *replace-me* ]]; then
        fail "$name still contains replace-me"
    elif [[ -z "$value" && ! ("$name" == PRODUCT_SEED && $unsealed == 1) ]]; then
        fail "$name is empty"
    fi
done
seed=${env[PRODUCT_SEED]-}
[[ -z "$seed" || "$seed" =~ ^[0-9a-f]{64}$ ]] ||
    fail "PRODUCT_SEED must be 64 lowercase hex characters (topup-sdk keygen --seed-out)"
if [[ -n "$os_image" && "$os_image" != "$approved_os_image" ]]; then
    fail "OS image $os_image is not the approved $approved_os_image (deploy/README.md)"
fi

echo "== compose"
if docker compose -f "$compose" config --no-interpolate --format json >"$tmp/compose.json" \
    2>"$tmp/compose.err"; then
    image=$(jq -r '.services.product.image' "$tmp/compose.json")
    printf '%s\n' "$image" >"$tmp/images"
    if ! [[ "$image" =~ ^[^@]+@sha256:[0-9a-f]{64}$ ]] || [[ "$image" == *@sha256:0000000000000000000000000000000000000000000000000000000000000000 ]]; then
        fail "image $image is not a nonzero repository@sha256 digest; run render-compose.sh"
    fi
    docker compose -f "$compose" config --variables 2>/dev/null |
        awk 'NR > 1 && NF > 0 { print $1 }' | sort >"$tmp/compose-variables"
    cmp -s "$tmp/compose-variables" "$tmp/expected" ||
        fail "the compose reads other variables than $example"
    # The public settings, from the attested product config.
    declare -A setting=()
    if jq -er '.configs.product_config.content' "$tmp/compose.json" >"$tmp/config.json" 2>/dev/null &&
        jq -e 'type == "object"' "$tmp/config.json" >/dev/null 2>&1; then
        for pair in TOPUP_ORIGIN=service_url PRODUCT_PUBLIC_URL=public_url PRODUCT_RPC_URL=rpc_url \
            PRODUCT_DRIVER_PUBLIC_KEY=driver_public_key; do
            setting[${pair%%=*}]=$(jq -r --arg key "${pair#*=}" '.[$key] // "" | strings' "$tmp/config.json")
        done
    else
        fail "the compose's product_config is not a JSON object"
    fi
    for name in TOPUP_ORIGIN PRODUCT_PUBLIC_URL; do
        [[ "${setting[$name]-}" =~ $origin_pattern ]] ||
            fail "$name must be https://HOST[:PORT] in lowercase with no path"
    done
    if [[ "${setting[PRODUCT_PUBLIC_URL]-}" == *.invalid ]]; then
        echo "note: PRODUCT_PUBLIC_URL is provisional; the gateway URL replaces it after provisioning"
    fi
    rpc=${setting[PRODUCT_RPC_URL]-}
    [[ "$rpc" == https://* ]] || fail "PRODUCT_RPC_URL must use https"
    driver_key_bytes=$(base64 -d 2>/dev/null <<<"${setting[PRODUCT_DRIVER_PUBLIC_KEY]-}" | wc -c) ||
        driver_key_bytes=0
    [[ "$driver_key_bytes" == 32 ]] || fail "PRODUCT_DRIVER_PUBLIC_KEY must be standard base64 of 32 bytes"
    if env PRODUCT_IMAGE="$image" TOPUP_ORIGIN="${setting[TOPUP_ORIGIN]-}" \
        PRODUCT_PUBLIC_URL="${setting[PRODUCT_PUBLIC_URL]-}" PRODUCT_RPC_URL="$rpc" \
        PRODUCT_DRIVER_PUBLIC_KEY="${setting[PRODUCT_DRIVER_PUBLIC_KEY]-}" \
        "$root/deploy/product/render-compose.sh" "$source_compose" >"$tmp/fresh.yml" 2>/dev/null &&
        cmp -s "$tmp/fresh.yml" "$compose"; then
        :
    else
        fail "$compose differs from a fresh render of $source_compose with its image and settings"
    fi
else
    fail "docker compose cannot parse $compose: $(head -c 300 "$tmp/compose.err")"
    : >"$tmp/images"
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

echo "== product RPC and topup (no RPC URL is printed)"
require_command cast
chain_id=$(ETH_RPC_URL=$rpc cast chain-id 2>/dev/null) || chain_id=error
[[ "$chain_id" == 11155111 ]] || fail "PRODUCT_RPC_URL reports chain id $chain_id, not Sepolia"
# finalized_block URL: the number of the RPC's finalized block, in decimal.
finalized_block() {
    local number
    number=$(ETH_RPC_URL=$1 cast rpc eth_getBlockByNumber finalized false 2>/dev/null |
        jq -er '.number | select(test("^0x[0-9a-f]{1,16}$"))' 2>/dev/null) || return 1
    echo $((number))
}
reference=0
for name in TOPUP_RPC_PROVIDER_A_URL TOPUP_RPC_PROVIDER_B_URL; do
    if [[ -z "${!name:-}" ]]; then
        fail "$name (topup's provider, the finality reference) is not set in the environment"
    elif number=$(finalized_block "${!name}"); then
        ((number > reference)) && reference=$number
    else
        fail "$name does not answer its finalized block"
    fi
done
if ! product_finalized=$(finalized_block "$rpc"); then
    fail "PRODUCT_RPC_URL does not answer its finalized block"
elif ((reference > 0)); then
    lag=$((reference - product_finalized))
    if ((lag > max_finalized_lag)); then
        fail "PRODUCT_RPC_URL's finalized block $product_finalized trails topup's providers ($reference)" \
            "by $lag blocks (at most $max_finalized_lag): the product would defer every settlement"
    else
        ok "PRODUCT_RPC_URL's finalized block is within $max_finalized_lag blocks of topup's providers"
    fi
fi
# The product pins the settlement key from this endpoint at startup and checks its binding.
nonce=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
if curl -fsS --max-time 30 "${setting[TOPUP_ORIGIN]}/v1/attestation?nonce=$nonce" >"$tmp/attestation.json" &&
    jq -e '.keyid == "settlement/v1" and (.settlement_pubkey | test("^[0-9a-f]{64}$"))' \
        "$tmp/attestation.json" >/dev/null; then
    ok "TOPUP_ORIGIN serves /v1/attestation with a settlement/v1 key"
else
    fail "TOPUP_ORIGIN does not serve a settlement/v1 attestation"
fi

check_phala_cloud "$workspace" "$os_image"

if ((failures)); then
    echo "preflight: $failures check(s) failed" >&2
    exit 1
fi
echo "preflight: all checks passed"
