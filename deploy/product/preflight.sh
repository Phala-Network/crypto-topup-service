#!/usr/bin/env bash
# Preflight for the staging reference-product CVM (deploy/README.md, "Staging reference product").
# Read-only against remote systems, like deploy/preflight.sh: it reads the env file, the compose
# rendered by deploy/product/render-compose.sh (which holds the public settings), the image in its
# registry, the product's RPC, topup's attestation endpoint, and the Phala Cloud account the CLI is
# logged in to.
#
# Usage: deploy/product/preflight.sh --env FILE --compose FILE --workspace NAME --os-image NAME
#          [--offline] [--unsealed]
#
# --offline runs only the local checks (env file and compose). --unsealed accepts an empty
# PRODUCT_API_KEY: Deploy (target `product`) provisions without it and the owner seals it from their own
# machine. PRODUCT_RPC_URL is published with the compose, so it must be keyless. Output never prints
# an RPC URL. Every failure is reported; the exit status is 1 if any.
set -euo pipefail
source "$(dirname -- "$0")/../contracts/common.sh"
source "$(dirname -- "$0")/../preflight-phala.sh"

root="$REPO_ROOT"
example="$root/deploy/product/staging.env.example"
source_compose="$root/deploy/product/docker-compose.yml"
approved_os_image=dstack-0.5.9

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
account=""
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
while IFS= read -r name; do
    value=${env[$name]-}
    if [[ "$value" == *replace-me* ]]; then
        fail "$name still contains replace-me"
    elif [[ -z "$value" && ! ("$name" == PRODUCT_API_KEY && $unsealed == 1) ]]; then
        fail "$name is empty"
    fi
done <"$tmp/expected"
api_key=${env[PRODUCT_API_KEY]-}
# 43 random base62 characters and a 6-character checksum (crates/topup/src/api_keys.rs). The
# staging product runs with a restricted test key; a secret test key is accepted too.
[[ -z "$api_key" || "$api_key" =~ ^ppay_(rk|sk)_test_[0-9A-Za-z]{49}$ ]] ||
    fail "PRODUCT_API_KEY must be a Phala Pay test API key (ppay_rk_test_… or ppay_sk_test_…)"
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
        account=$(jq -r '.account // "" | strings' "$tmp/config.json")
    else
        fail "the compose's product_config is not a JSON object"
    fi
    # The product's Phala Pay account: the account its webhooks and attestation must name, and the
    # first input of every address it pins.
    [[ "${account-}" =~ ^acct_[0-9a-f]{32}$ ]] ||
        fail "the product config's account must be the product's acct_ id (32 lowercase hex digits)"
    for name in TOPUP_ORIGIN PRODUCT_PUBLIC_URL; do
        [[ "${setting[$name]-}" =~ $origin_pattern ]] ||
            fail "$name must be https://HOST[:PORT] in lowercase with no path"
    done
    if [[ "${setting[PRODUCT_PUBLIC_URL]-}" == *.invalid ]]; then
        echo "note: PRODUCT_PUBLIC_URL is provisional; the gateway URL replaces it after provisioning"
    fi
    rpc=${setting[PRODUCT_RPC_URL]-}
    [[ "$rpc" == https://* ]] || fail "PRODUCT_RPC_URL must use https"
    # The staging-only product has no sealed RPC key: its URL is published and must be keyless.
    ! embeds_key "$rpc" ||
        fail "PRODUCT_RPC_URL seems to embed an API key, which the compose publishes; use a keyless URL"
    driver_key_bytes=$(base64 -d 2>/dev/null <<<"${setting[PRODUCT_DRIVER_PUBLIC_KEY]-}" | wc -c) ||
        driver_key_bytes=0
    [[ "$driver_key_bytes" == 32 ]] || fail "PRODUCT_DRIVER_PUBLIC_KEY must be standard base64 of 32 bytes"
    if env PRODUCT_IMAGE="$image" TOPUP_ORIGIN="${setting[TOPUP_ORIGIN]-}" \
        PRODUCT_PUBLIC_URL="${setting[PRODUCT_PUBLIC_URL]-}" PRODUCT_RPC_URL="$rpc" \
        PRODUCT_DRIVER_PUBLIC_KEY="${setting[PRODUCT_DRIVER_PUBLIC_KEY]-}" \
        "$root/deploy/product/render-compose.sh" "$source_compose" >"$tmp/fresh.yml" 2>"$tmp/render.err"; then
        cmp -s "$tmp/fresh.yml" "$compose" ||
            fail "$compose differs from a fresh render of $source_compose with its image and settings"
    else
        # render-compose.sh names only the variable, never a value.
        fail "$source_compose does not render with the settings of $compose: $(tool_error "$tmp/render.err")"
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

echo "== product account"
[[ "$account" != acct_00000000000000000000000000000000 ]] ||
    fail "the product config's account is the placeholder; commit the product's acct_ id to $source_compose"

echo "== product RPC and topup (no RPC URL is printed)"
require_command cast
if ! chain_id=$(ETH_RPC_URL=$rpc cast chain-id 2>"$tmp/cast.err"); then
    error=$(tool_error "$tmp/cast.err")
    chain_id="error: ${error//"$rpc"/PRODUCT_RPC_URL}"
fi
[[ "$chain_id" == 11155111 ]] || fail "PRODUCT_RPC_URL reports chain id $chain_id, not Sepolia"
# The product pins its account's webhook keys from this endpoint, fetched with its API key, and
# checks their binding. Without the key sealed yet, the endpoint must refuse anonymous calls.
nonce=$(head -c 32 /dev/urandom | od -An -tx1 | tr -d ' \n')
attestation_url="${setting[TOPUP_ORIGIN]}/v1/attestation?nonce=$nonce"
if [[ -n "$api_key" ]]; then
    if curl -fsS --max-time 30 -H @- "$attestation_url" >"$tmp/attestation.json" \
        <<<"Authorization: Bearer $api_key" &&
        jq -e --arg account "$account" '.livemode == false and .account == $account
            and (.webhook_keys[0].public_key | test("^[0-9a-f]{64}$"))' \
            "$tmp/attestation.json" >/dev/null; then
        ok "TOPUP_ORIGIN attests the product account's test-mode webhook key"
    else
        fail "TOPUP_ORIGIN does not attest a test-mode webhook key of the config's account for PRODUCT_API_KEY"
    fi
elif [[ "$(curl -sS --max-time 30 -o /dev/null -w '%{http_code}' "$attestation_url")" == 401 ]]; then
    ok "TOPUP_ORIGIN serves /v1/attestation to API keys only (PRODUCT_API_KEY not sealed yet)"
else
    fail "TOPUP_ORIGIN does not serve an authenticated /v1/attestation"
fi

check_phala_cloud "$workspace" "$os_image"

if ((failures)); then
    echo "preflight: $failures check(s) failed" >&2
    exit 1
fi
echo "preflight: all checks passed"
