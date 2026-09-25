#!/usr/bin/env bash
# Renders the reference-product compose for a CVM (deploy/README.md, "Staging reference product"):
# PRODUCT_IMAGE to a digest with deploy/render-compose.sh, the public settings below inline from
# the environment, and the service label crypto-topup.rendered-sha256 to the digest of the rendered
# file, so a changed setting changes the attested compose and recreates the container. The output
# reads only PRODUCT_SEED from the env. Values are never printed: errors name only the variable.
#
# Usage: deploy/product/render-compose.sh [SOURCE_COMPOSE] >docker-compose.product.yml
set -euo pipefail
export LC_ALL=C

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
source_compose=${1:-"$root/deploy/product/docker-compose.yml"}
settings=(TOPUP_ORIGIN PRODUCT_PUBLIC_URL PRODUCT_RPC_URL PRODUCT_DRIVER_PUBLIC_KEY)

# Values land inside JSON strings in a YAML block scalar that Compose interpolates: allow printable
# ASCII without spaces, quotes, backslashes, or `$`.
for name in "${settings[@]}"; do
    value=${!name-}
    if ! [[ "$value" =~ ^[[:graph:]]{1,512}$ ]] || [[ "$value" == *[\"\\\$]* ]]; then
        echo "render-compose.sh: $name must be 1-512 printable ASCII characters without spaces," \
            "quotes, backslashes, or \$" >&2
        exit 64
    fi
done

rest=$("$root/deploy/render-compose.sh" "$source_compose")
# Single pass: only the settings' `${NAME:-}` placeholders are replaced; values are not rescanned.
rendered=""
while [[ "$rest" =~ \$\{([A-Z_]+):-\} ]]; do
    placeholder=${BASH_REMATCH[0]}
    name=${BASH_REMATCH[1]}
    rendered+=${rest%%"$placeholder"*}
    if [[ " ${settings[*]} " == *" $name "* ]]; then
        rendered+=${!name}
    else
        rendered+=$placeholder
    fi
    rest=${rest#*"$placeholder"}
done
rendered+=$rest

label='${PRODUCT_RENDERED_SHA256:-}'
[[ "$rendered" == *"$label"* ]] || {
    echo "render-compose.sh: $source_compose lacks the $label label" >&2
    exit 64
}
# The owner runs the offline preflight, and so this renderer, on their own machine (macOS: shasum).
if command -v sha256sum >/dev/null; then sha256=(sha256sum); else sha256=(shasum -a 256); fi
digest=$(printf '%s\n' "$rendered" | "${sha256[@]}" | awk '{print $1}')
rendered=${rendered/"$label"/$digest}
left=$(grep -o '[$][{][^}]*[}]' <<<"$rendered" | sort -u)
[[ "$left" == '${PRODUCT_SEED:-}' ]] || {
    echo "render-compose.sh: the rendered compose must read only PRODUCT_SEED from the env" >&2
    exit 1
}
printf '%s\n' "$rendered"
