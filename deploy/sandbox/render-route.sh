#!/usr/bin/env bash
# Renders routes/sandbox-sepolia.template.yaml from environment variables and refuses to emit a
# file with any unsubstituted placeholder. Usage: render-route.sh > route.yaml
set -euo pipefail
export LC_ALL=C

template="$(dirname -- "$0")/routes/sandbox-sepolia.template.yaml"
: "${RATE_LOCK_WINDOW_S:=120}"
export RATE_LOCK_WINDOW_S
for name in FORWARDER_FACTORY IMPLEMENTATION TREASURY TEST_TOKEN SANCTIONS_ORACLE \
    PRODUCT_SLUG PRODUCT_KID SETTLEMENT_URL; do
    [[ -n "${!name:-}" ]] || { echo "render-route.sh: $name is required" >&2; exit 1; }
done
for name in FORWARDER_FACTORY IMPLEMENTATION TREASURY TEST_TOKEN SANCTIONS_ORACLE; do
    [[ "${!name}" =~ ^0x[0-9a-fA-F]{40}$ ]] || {
        echo "render-route.sh: $name must be a 0x-prefixed 20-byte hex address" >&2
        exit 1
    }
done
[[ "$PRODUCT_SLUG" =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || {
    echo "render-route.sh: PRODUCT_SLUG must be lowercase letters, digits, and dashes" >&2
    exit 1
}
# Values land inside double-quoted YAML scalars: allow printable ASCII without quotes,
# backslashes, or `$`, matching issue-product.sh.
for name in PRODUCT_KID SETTLEMENT_URL; do
    value=${!name}
    if ! [[ "$value" =~ ^[[:print:]]{1,512}$ ]] || [[ "$value" == *[\"\\\$]* ]]; then
        echo "render-route.sh: $name must be printable ASCII without quotes, backslashes, or \$" >&2
        exit 1
    fi
done
[[ ${#PRODUCT_KID} -le 128 ]] || { echo "render-route.sh: PRODUCT_KID is too long" >&2; exit 1; }

# Single pass over the template: only allow-listed `${NAME}` placeholders are replaced, and
# substituted values are never rescanned. Unlike envsubst, bare `$NAME` is not supported; any
# `$NAME` or `${...}` left in the output (from the template or a value) is rejected below.
allowed=" FORWARDER_FACTORY IMPLEMENTATION TREASURY TEST_TOKEN SANCTIONS_ORACLE PRODUCT_SLUG \
PRODUCT_KID SETTLEMENT_URL RATE_LOCK_WINDOW_S "
rest="$(<"$template")"
rendered=""
while [[ "$rest" =~ \$\{([A-Za-z_][A-Za-z0-9_]*)\} ]]; do
    placeholder=${BASH_REMATCH[0]}
    name=${BASH_REMATCH[1]}
    rendered+=${rest%%"$placeholder"*}
    if [[ "$allowed" == *" $name "* ]]; then
        rendered+=${!name}
    else
        rendered+=$placeholder
    fi
    rest=${rest#*"$placeholder"}
done
rendered+=$rest
if grep -q '\$[A-Za-z_{]' <<<"$rendered"; then
    echo "render-route.sh: unsubstituted placeholder in output" >&2
    exit 1
fi
printf '%s\n' "$rendered"
