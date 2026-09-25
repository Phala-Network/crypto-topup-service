#!/usr/bin/env bash
# Writes the unsealed staging env file from the process environment: exactly the names of
# deploy/staging.env.example, each with the value of the environment variable of the same name,
# except the owner-sealed secrets, which are always written empty (never read from the
# environment). The Deploy staging workflow maps its GitHub Environment variables to those names;
# the owner later seals the complete file from their own machine (deploy/README.md).
#
# Usage: deploy/write-staging-env.sh [--product] OUTPUT
#
# --product writes the reference product's env file instead: its only name, the secret
# PRODUCT_SEED (deploy/product/staging.env.example), empty. The product's public settings are not
# env values; deploy/product/render-compose.sh renders them into the attested compose.
#
# OUTPUT must already exist (create it with mktemp, mode 0600); it is overwritten. Every name must
# be set and non-empty, except the ones preflight.sh --unsealed allows to be empty. A value must be one line
# without quotes, backticks, or `#`, which the Phala CLI's dotenv parser would strip or treat as a
# comment. Values are never printed: errors name only the variable.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
example="$root/deploy/staging.env.example"
# Same lists as preflight.sh.
optional_empty=" AWS_SESSION_TOKEN AWS_ENDPOINT COINMETRICS_API_KEY "
owner_sealed=" AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN COINMETRICS_API_KEY "

if [[ "${1:-}" == --product ]]; then
    example="$root/deploy/product/staging.env.example"
    optional_empty=" "
    owner_sealed=" PRODUCT_SEED "
    shift
fi
(($# == 1)) || { echo "usage: $0 [--product] OUTPUT" >&2; exit 64; }
output=$1
[[ -f "$output" ]] || { echo "$output must exist (create it with mktemp)" >&2; exit 64; }

missing=() invalid=()
lines=()
while IFS= read -r name; do
    if [[ "$owner_sealed" == *" $name "* ]]; then
        lines+=("$name=")
        continue
    fi
    value=${!name-}
    if [[ -z "$value" && "$optional_empty" != *" $name "* ]]; then
        missing+=("$name")
    elif [[ "$value" == *[$'\n\r"\'`#']* ]]; then
        invalid+=("$name")
    else
        lines+=("$name=$value")
    fi
done < <(awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$example")

if ((${#missing[@]})); then
    echo "missing or empty: ${missing[*]}" >&2
fi
if ((${#invalid[@]})); then
    echo "values must be one line without quotes, backticks, or #: ${invalid[*]}" >&2
fi
((${#missing[@]} + ${#invalid[@]} == 0)) || exit 1

chmod 600 "$output"
printf '%s\n' "${lines[@]}" >"$output"
echo "wrote ${#lines[@]} names to $output"
