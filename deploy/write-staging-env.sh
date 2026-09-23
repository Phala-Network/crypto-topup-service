#!/usr/bin/env bash
# Writes the staging env file from the process environment: exactly the names of
# deploy/staging.env.example, each with the value of the environment variable of the same name.
# The Deploy staging workflow maps its GitHub Environment secrets and variables to those names.
#
# Usage: deploy/write-staging-env.sh OUTPUT
#
# OUTPUT must already exist (create it with mktemp, mode 0600); it is overwritten. Every name must
# be set and non-empty, except the ones preflight.sh allows to be empty. A value must be one line
# without quotes, backticks, or `#`, which the Phala CLI's dotenv parser would strip or treat as a
# comment. Values are never printed: errors name only the variable.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
example="$root/deploy/staging.env.example"
# Same list as preflight.sh.
optional_empty=" AWS_SESSION_TOKEN AWS_ENDPOINT COINMETRICS_API_KEY "

(($# == 1)) || { echo "usage: $0 OUTPUT" >&2; exit 64; }
output=$1
[[ -f "$output" ]] || { echo "$output must exist (create it with mktemp)" >&2; exit 64; }

missing=() invalid=()
lines=()
while IFS= read -r name; do
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
