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

rendered="$(envsubst '${FORWARDER_FACTORY} ${IMPLEMENTATION} ${TREASURY} ${TEST_TOKEN}
    ${SANCTIONS_ORACLE} ${PRODUCT_SLUG} ${PRODUCT_KID} ${SETTLEMENT_URL} ${RATE_LOCK_WINDOW_S}' \
    <"$template")"
if grep -q '\${' <<<"$rendered"; then
    echo "render-route.sh: unsubstituted placeholder in output" >&2
    exit 1
fi
printf '%s\n' "$rendered"
