#!/usr/bin/env bash
# Preflight for a staging deploy (deploy/README.md, "First staging deploy checklist"). It is
# read-only against remote systems: it never pushes, deploys, updates, or sends a transaction. It
# reads the env file (the owner-sealed secrets), the rendered compose (which holds the public
# settings, deploy/README.md "Attested settings"), the images in their registry, each route's
# asset chain through every RPC provider the route names, and the Phala Cloud account the CLI is
# logged in to.
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
# file, compose, routes). --unsealed accepts empty owner-sealed secrets: Deploy provisions
# with them empty and the owner seals them from their own machine; check that file without
# --unsealed. PHALA selects the CLI command (default `npx --yes phala@1.1.22`). Every failure is
# reported; the exit status is 1 if any.
#
# Every route config of the compose (`topup_route_*`) is checked. Each route names its chain's RPC
# providers by id (`chain.rpc_providers`, default provider-a and provider-b), and the compose
# carries each provider's URL, TOPUP_RPC_<ID>_URL (deploy/README.md, "RPC providers"): every
# provider a route names, and no other, on one chain only, each route's providers different URLs.
# The URLs are attested and published with the compose, so they must not embed an API key: a
# keyed provider's URL has `{key}` where the key goes, and the key is the owner-sealed
# TOPUP_RPC_<ID>_KEY of staging.env.example (empty for a keyless URL), filled in here for the
# online checks: each provider must report the chain of each route that names it. Without the keys
# (--unsealed) the asset chain checks are skipped: run preflight online with the sealed env file for
# them. Output never prints a URL or key: tool errors are redacted to "provider ID".
set -euo pipefail
source "$(dirname -- "$0")/contracts/common.sh"
source "$(dirname -- "$0")/preflight-phala.sh"

root="$REPO_ROOT"
example="$root/deploy/staging.env.example"
networks="$DEPLOY_CONTRACTS_DIR/networks.json"
# May stay empty: an empty DSN turns Sentry reporting off, as does an empty TOPUP_RPC_<ID>_KEY
# mean a keyless URL.
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

# provider_variable ID SUFFIX: the provider's variable TOPUP_RPC_<ID>_<SUFFIX>, the id upper-cased
# with `-` as `_` (crates/topup/src/rpc_provider.rs). Ids are lowercase letters, digits, and `-`,
# so provider_id inverts it.
provider_variable() {
    local id=${1^^}
    printf 'TOPUP_RPC_%s_%s' "${id//-/_}" "$2"
}
provider_id() {
    local id=${1#TOPUP_RPC_}
    id=${id%_URL}
    id=${id,,}
    printf '%s' "${id//_/-}"
}

# route_value FILE KEY: the first value of KEY in the route file.
route_value() {
    sed -n "s/^[[:space:]]*$2:[[:space:]]*\"\{0,1\}\([^\"#[:space:]]*\)\"\{0,1\}.*/\1/p" "$1" |
        head -n 1
}
# route_providers FILE: the route's `chain.rpc_providers` (a flow or block sequence), one line, or
# provider-a and provider-b when it names none (crates/core/src/route.rs). Online, topup's own
# reading (`topup route show`) must agree.
route_providers() {
    awk '
        /^[^[:space:]#]/ { if (block) exit; chain = /^chain:/ }
        chain && !block && /^[[:space:]]+rpc_providers:/ {
            found = 1
            line = $0
            sub(/^[^:]*:[[:space:]]*/, "", line)
            sub(/[[:space:]]*#.*/, "", line)
            if (line == "") { block = 1; next }
            gsub(/[][ "\047]/, "", line)
            count = split(line, ids, ",")
            for (i = 1; i <= count; i++) if (ids[i] != "") printf "%s%s", (printed++ ? " " : ""), ids[i]
            exit
        }
        block && /^[[:space:]]*($|#)/ { next }
        block && /^[[:space:]]+-/ {
            id = $0
            sub(/^[[:space:]]+-[[:space:]]*/, "", id)
            sub(/[[:space:]]*#.*/, "", id)
            gsub(/["\047]/, "", id)
            printf "%s%s", (printed++ ? " " : ""), id
            next
        }
        block { exit }
        END { if (!found) printf "provider-a provider-b"; print "" }
    ' "$1"
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
    elif [[ -z "$value" ]] && ((unsealed == 0)) && [[ "$optional_empty" != *" $name "* ]] &&
        [[ "$name" != TOPUP_RPC_*_KEY ]]; then
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
# provider_url[ID]: each RPC provider's URL, with its key where the env file has it.
declare -A provider_url=()
route_configs=()
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
    # Every route config, one file each.
    mkdir "$tmp/routes"
    mapfile -t route_configs < <(jq -r '.configs // {} | keys[] | select(startswith("topup_route_"))' \
        "$tmp/compose.json")
    for config in "${route_configs[@]}"; do
        jq -j --arg name "$config" '.configs[$name].content // empty' "$tmp/compose.json" \
            >"$tmp/routes/$config.yaml"
    done
    # The public settings, from the attested compose, with every RPC provider's URL.
    declare -A setting=()
    while IFS=$'\t' read -r name value; do
        setting[$name]=$value
    done < <(jq -r '.services as $s | ($s.topup.environment + $s.postgres.environment) as $e
        | ((["TOPUP_ADMIN_KID", "TOPUP_ADMIN_PUBLIC_KEY", "TOPUP_PUBLIC_ORIGIN",
            "TOPUP_SERVICE_ENABLED", "WALG_S3_PREFIX", "AWS_ENDPOINT", "AWS_REGION",
            "AWS_S3_FORCE_PATH_STYLE", "TOPUP_RESTORE_FROM_BACKUP"][]
            | [., ($e[.] // "" | strings)]),
          ($s.topup.environment | keys[] | select(test("^TOPUP_RPC_[A-Z0-9_]+_URL$"))
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
    # Each RPC provider's URL, by the same rules as topup's (crates/topup/src/rpc_provider.rs), and
    # then with its key for the online checks.
    for url_name in "${!setting[@]}"; do
        [[ "$url_name" == TOPUP_RPC_*_URL ]] || continue
        id=$(provider_id "$url_name") key_name=${url_name%_URL}_KEY
        url=${setting[$url_name]} key=${env[$key_name]-}
        [[ "$url" == https://* ]] || fail "$url_name must use https"
        embeds_key "$url" &&
            fail "$url_name seems to embed an API key, which the compose publishes; attest it with" \
                "{key} in the key's place and seal the key as $key_name"
        if [[ "$url" != *"{key}"* ]]; then
            [[ -z "$key" ]] || fail "$key_name is set, but $url_name has no {key} placeholder"
        elif ! grep -qx "$key_name" "$tmp/expected"; then
            fail "$url_name has a {key} placeholder, but staging.env.example and the compose have" \
                "no $key_name to seal its key in"
        elif [[ -z "$key" ]]; then
            ((unsealed)) || fail "$key_name is required by the {key} placeholder of $url_name"
        elif ! [[ "$key" =~ ^[A-Za-z0-9._~-]{8,}$ ]]; then
            fail "$key_name must be at least 8 characters of A-Z, a-z, 0-9, and -._~"
        fi
        [[ -z "$key" ]] || url=${url//"{key}"/"$key"}
        provider_url[$id]=$url
    done
    while IFS= read -r key_name; do
        [[ -v "setting[${key_name%_KEY}_URL]" ]] ||
            fail "$key_name is in staging.env.example, but the compose has no ${key_name%_KEY}_URL"
    done < <(grep -x 'TOPUP_RPC_[A-Z0-9_]*_KEY' "$tmp/expected")
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
        "$root/deploy/render-compose.sh" "${variant[@]}" "$source_compose" >"$tmp/fresh.yml" \
        2>"$tmp/render.err"; then
        cmp -s "$tmp/fresh.yml" "$compose" ||
            fail "$compose differs from a fresh render of $source_compose with the same images and" \
                "settings; re-run render-compose.sh from the commit being deployed"
    else
        # render-compose.sh names only the variable, never a value.
        fail "$source_compose does not render with the settings of $compose: $(tool_error "$tmp/render.err")"
    fi
else
    fail "docker compose cannot parse $compose: $(head -c 300 "$tmp/compose.err")"
    : >"$tmp/images"
fi

echo "== routes"
# route[CONFIG/KEY]: a route's values; route_ids[CONFIG]: its providers; chain_of[ID]: the chain of
# the routes that name a provider.
declare -A route=() route_ids=() chain_of=()
for config in "${route_configs[@]}"; do
    file=$tmp/routes/$config.yaml
    # The file names only what differs per route; the implementation (the factory's first CREATE)
    # and, on chains with a Chainalysis oracle, the sanctions oracle are code defaults that the
    # online checks read from `topup route show`.
    for key in route chain_id forwarder_factory contract sanctions_oracle decimals; do
        route[$config/$key]=$(route_value "$file" "$key")
    done
    name=${route[$config/route]} chain_id=${route[$config/chain_id]}
    [[ -n "$name" ]] || fail "the attested $config names no route"
    for key in forwarder_factory contract sanctions_oracle; do
        address=${route[$config/$key]}
        if [[ "$key" == sanctions_oracle && -z "$address" ]]; then
            continue
        elif ! is_address "$address"; then
            fail "route $name: $key is not an address: '$address'"
        elif placeholder_address "$address"; then
            fail "route $name: $key is the placeholder or zero address $address; deploy the" \
                "contracts (deploy/CONTRACTS.md) and commit the real address"
        fi
    done
    route_ids[$config]=$(route_providers "$file")
    read -ra ids <<<"${route_ids[$config]}"
    checked=()
    for id in "${ids[@]}"; do
        url_name=$(provider_variable "$id" URL)
        if ! [[ "$id" =~ ^[a-z0-9-]+$ ]]; then
            fail "route $name: RPC provider '$id' is not a provider id (lowercase letters, digits, -)"
            continue
        elif ! [[ -v "provider_url[$id]" ]]; then
            fail "route $name names RPC provider $id, but the compose has no $url_name"
            continue
        fi
        for other in "${checked[@]}"; do
            [[ "${setting[$url_name]}" != "${setting[$(provider_variable "$other" URL)]}" ]] ||
                fail "route $name: RPC providers $other and $id have the same URL; a chain's" \
                    "providers must be different providers"
        done
        checked+=("$id")
        [[ "${chain_of[$id]-$chain_id}" == "$chain_id" ]] ||
            fail "RPC provider $id is named on chain ${chain_of[$id]} and chain $chain_id; each" \
                "chain needs providers of its own"
        chain_of[$id]=$chain_id
    done
done
((${#route_configs[@]})) || fail "the compose has no inline topup_route_* config"
for id in "${!provider_url[@]}"; do
    [[ -v "chain_of[$id]" ]] ||
        fail "the compose has $(provider_variable "$id" URL), but no route names provider $id"
done

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
for config in "${route_configs[@]}"; do
    name=${route[$config/route]}
    if docker run --rm -i --pull never "$topup_image" topup route show /dev/stdin \
        <"$tmp/routes/$config.yaml" \
        >"$tmp/routes/$config.json" 2>"$tmp/validate.out"; then
        ok "topup route show resolves route $name"
        route[$config/implementation]=$(jq -r '.chain.implementation' "$tmp/routes/$config.json")
        route[$config/sanctions_oracle]=$(jq -r '.chain.sanctions_oracle' "$tmp/routes/$config.json")
        providers=$(jq -r '.chain.rpc_providers | join(" ")' "$tmp/routes/$config.json")
        [[ "$providers" == "${route_ids[$config]}" ]] ||
            fail "route $name: preflight read chain.rpc_providers as '${route_ids[$config]}', but" \
                "topup as '$providers'; write them as a list of provider ids"
    else
        fail "topup route show rejected route $name: $(tail -n 3 "$tmp/validate.out")"
    fi
done

echo "== asset chains (RPC URLs are not printed)"
if [[ "${provider_url[*]}" == *"{key}"* ]]; then
    echo "note: skipped: a keyed RPC provider has no key here (--unsealed); run preflight online" \
        "with the sealed env file to check the asset chains"
else
    for command in cast forge; do
        require_command "$command"
    done
    redact() {
        local text=$1 id key
        for id in "${!provider_url[@]}"; do
            text=${text//"${provider_url[$id]}"/provider $id}
        done
        for key in "${!env[@]}"; do
            [[ "$key" == TOPUP_RPC_*_KEY && -n "${env[$key]}" ]] && text=${text//"${env[$key]}"/[key]}
        done
        printf '%s' "$text"
    }
    # rpc URL CAST_ARGS...: cast's answer from the provider at URL, or "error: " and its redacted error.
    rpc() {
        local url=$1
        shift
        ETH_RPC_URL=$url cast "$@" 2>"$tmp/cast.err" ||
            printf 'error: %s' "$(redact "$(tool_error "$tmp/cast.err")")"
    }
    # Each route on each provider it names: the chain, the contracts, and the asset.
    for config in "${route_configs[@]}"; do
        name=${route[$config/route]} chain_id=${route[$config/chain_id]}
        read -ra ids <<<"${route_ids[$config]}"
        chain_ok=1
        for id in "${ids[@]}"; do
            reported=$(rpc "${provider_url[$id]}" chain-id)
            if [[ "$reported" == "$chain_id" ]]; then
                ok "provider $id reports chain id $reported (route $name)"
            else
                fail "provider $id reports chain id $reported, route $name needs $chain_id"
                chain_ok=0
            fi
        done
        ((chain_ok)) || continue
        network=$(jq -r --argjson id "$chain_id" \
            '.networks | to_entries[] | select(.value.chain_id == $id) | .key' "$networks")
        if [[ -z "$network" ]]; then
            fail "$networks names no network with chain id $chain_id (route $name)"
            continue
        fi
        # Once per chain: a chain's routes name the same providers.
        verification=$tmp/verification-$chain_id.json
        if ! [[ -e "$verification" ]]; then
            targets=()
            for id in "${ids[@]}"; do
                targets+=(--rpc "$network/$id=${provider_url[$id]}")
            done
            if "$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" "${targets[@]}" >"$verification" \
                2>"$tmp/verification.err"; then
                ok "verify-deployment.sh passed on every provider of chain $chain_id"
            else
                fail "verify-deployment.sh failed on chain $chain_id:" \
                    "$(redact "$(tail -n 3 "$tmp/verification.err")")"
            fi
        fi
        if jq -e --argjson count "${#ids[@]}" --arg factory "${route[$config/forwarder_factory]}" \
            --arg implementation "${route[$config/implementation]-}" \
            '(.chains | length) == $count and all(.chains[];
                (.factory | ascii_downcase) == ($factory | ascii_downcase) and
                (.implementation | ascii_downcase) == ($implementation | ascii_downcase))' \
            "$verification" >/dev/null 2>&1; then
            ok "route $name factory and implementation match the verified deployment"
        else
            fail "route $name contract addresses differ from the verified deployment"
        fi
        for id in "${ids[@]}"; do
            for key in contract sanctions_oracle; do
                address=${route[$config/$key]}
                code=$(rpc "${provider_url[$id]}" code "$address")
                [[ "$code" =~ ^0x[0-9a-fA-F]+$ && "$code" != 0x ]] ||
                    fail "route $name: $key $address has no code on provider $id (${code:0:300})"
            done
        done
        decimals=$(rpc "${provider_url[${ids[0]}]}" call "${route[$config/contract]}" 'decimals()(uint8)')
        [[ "$decimals" == "${route[$config/decimals]}" ]] ||
            fail "route $name: asset decimals() is $decimals, the route says ${route[$config/decimals]}"
    done
fi

check_phala_cloud "$workspace" "$os_image"

if ((failures)); then
    echo "preflight: $failures check(s) failed" >&2
    exit 1
fi
echo "preflight: all checks passed"
