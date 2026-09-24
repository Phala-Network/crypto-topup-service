#!/usr/bin/env bash
# Local (offline) preflight checks: the example env file, a zero-address route, and
# a stale render must be refused, and a complete env file with a filled route must pass.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
preflight="$root/deploy/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:1111111111111111111111111111111111111111111111111111111111111111
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:2222222222222222222222222222222222222222222222222222222222222222
# A source compose whose inline route still has zero-address placeholders, as before the route PR.
sed -E 's/((forwarder_factory|implementation|treasury|contract|sanctions_oracle): )"0x[0-9a-fA-F]{40}"/\1"0x0000000000000000000000000000000000000000"/' \
    "$root/deploy/docker-compose.yml" >"$tmp/zero-source.yml"
"$root/deploy/render-compose.sh" "$tmp/zero-source.yml" >"$tmp/zero-route.yml"
# The committed compose carries the deployed route addresses.
cp "$root/deploy/docker-compose.yml" "$tmp/filled-source.yml"
"$root/deploy/render-compose.sh" "$tmp/filled-source.yml" >"$tmp/filled-route.yml"

awk -F= '
    /^[[:space:]]*($|#)/ { next }
    $1 == "TOPUP_ADMIN_PUBLIC_KEY" { print $1 "=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo="; next }
    $1 == "TOPUP_PUBLIC_ORIGIN" { print $1 "=https://pending.invalid"; next }
    $1 == "TOPUP_RPC_PROVIDER_A_URL" { print $1 "=https://rpc-a.example/sepolia"; next }
    $1 == "TOPUP_RPC_PROVIDER_B_URL" { print $1 "=https://rpc-b.example/sepolia"; next }
    $1 == "WALG_S3_PREFIX" { print $1 "=s3://topup-staging/postgres"; next }
    $1 == "AWS_SESSION_TOKEN" || $1 == "COINMETRICS_API_KEY" { print $1 "="; next }
    $2 == "replace-me" { print $1 "=staging-value"; next }
    { print }
' "$root/deploy/staging.env.example" >"$tmp/complete.env"

# expect_failure NAME EXPECTED_MESSAGE ARGS...: preflight must fail and print the message.
expect_failure() {
    local name=$1 message=$2
    shift 2
    if "$preflight" "$@" --offline >"$tmp/$name.out" 2>"$tmp/$name.err"; then
        echo "preflight accepted $name" >&2
        exit 1
    fi
    grep -F -- "$message" "$tmp/$name.err" >/dev/null || {
        echo "preflight rejected $name for an unexpected reason:" >&2
        cat "$tmp/$name.err" >&2
        exit 1
    }
}

expect_failure example-env "TOPUP_PUBLIC_ORIGIN still contains replace-me" \
    --env "$root/deploy/staging.env.example" --compose "$tmp/zero-route.yml" \
    --source "$tmp/zero-source.yml"
expect_failure zero-route \
    "route forwarder_factory is the placeholder or zero address 0x0000000000000000000000000000000000000000" \
    --env "$tmp/complete.env" --compose "$tmp/zero-route.yml" --source "$tmp/zero-source.yml"
if grep -v 'placeholder or zero address' "$tmp/zero-route.err" | grep -q '^FAIL'; then
    echo "the complete env file failed a check:" >&2
    cat "$tmp/zero-route.err" >&2
    exit 1
fi
echo "EXTRA_SECRET=x" >>"$tmp/extra.env"
cat "$tmp/complete.env" >>"$tmp/extra.env"
expect_failure extra-name "names outside staging.env.example: EXTRA_SECRET" \
    --env "$tmp/extra.env" --compose "$tmp/zero-route.yml" --source "$tmp/zero-source.yml"
# A render that does not match its source (stale or hand-edited) is refused.
expect_failure stale-render "differs from a fresh render" \
    --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" --source "$tmp/zero-source.yml"

"$preflight" --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" \
    --source "$tmp/filled-source.yml" --offline >/dev/null

echo "preflight local checks test passed"
