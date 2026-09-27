#!/usr/bin/env bash
# Local (offline) preflight checks: the example env file, a zero-address route, a stale render, a
# source that does not render, the wrong variant, invalid settings, and an OS image other than the
# approved one must be refused, an RPC key must fit its URL and never be published or printed,
# and a complete env file with a filled route must pass.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
preflight="$root/deploy/preflight.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

export TOPUP_IMAGE=ghcr.io/phala-network/phala-pay@sha256:1111111111111111111111111111111111111111111111111111111111111111
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:2222222222222222222222222222222222222222222222222222222222222222
# The public settings, as Deploy passes them from the `staging` Environment variables.
export AWS_ENDPOINT=https://account.r2.cloudflarestorage.com AWS_REGION=auto
export AWS_S3_FORCE_PATH_STYLE=false WALG_S3_PREFIX=s3://topup-staging/postgres
export TOPUP_ADMIN_KID=staging-admin/v1 TOPUP_ADMIN_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
export SENTRY_ENVIRONMENT=staging
export TOPUP_DOMAIN=pay-api-staging.phala.com TOPUP_GATEWAY_DOMAIN=gateway.dstack-pha-prod5.phala.network
export TOPUP_RPC_PROVIDER_A_URL=https://rpc-a.example/sepolia TOPUP_RPC_PROVIDER_B_URL=https://rpc-b.example/sepolia
# A source compose whose inline route still has zero-address placeholders, as before the route PR.
sed -E 's/((forwarder_factory|implementation|treasury|contract|sanctions_oracle): )"0x[0-9a-fA-F]{40}"/\1"0x0000000000000000000000000000000000000000"/' \
    "$root/deploy/docker-compose.yml" >"$tmp/zero-source.yml"
"$root/deploy/render-compose.sh" "$tmp/zero-source.yml" >"$tmp/zero-route.yml"
# The committed compose carries the deployed route addresses.
cp "$root/deploy/docker-compose.yml" "$tmp/filled-source.yml"
"$root/deploy/render-compose.sh" "$tmp/filled-source.yml" >"$tmp/filled-route.yml"

awk -F= '
    /^[[:space:]]*($|#)/ { next }
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

expect_failure example-env "AWS_ACCESS_KEY_ID still contains replace-me" \
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
# A source that does not render is reported with render-compose.sh's own error.
grep -v '_RENDERED_SHA256:-' "$tmp/filled-source.yml" >"$tmp/unlabeled-source.yml"
expect_failure bad-source "does not render with the settings of $tmp/filled-route.yml:" \
    --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" --source "$tmp/unlabeled-source.yml"
grep -qF 'must carry exactly one ${..._RENDERED_SHA256:-} label' "$tmp/bad-source.err" || {
    echo "preflight did not print render-compose.sh's error:" >&2
    cat "$tmp/bad-source.err" >&2
    exit 1
}

"$preflight" --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" \
    --source "$tmp/filled-source.yml" --offline >/dev/null

# The restore-check variant passes only with --restore-check, and the service variant only without.
"$root/deploy/render-compose.sh" --restore-check "$tmp/filled-source.yml" >"$tmp/restore-check.yml"
expect_failure restore-check-as-service "the compose is not the service variant" \
    --env "$tmp/complete.env" --compose "$tmp/restore-check.yml" --source "$tmp/filled-source.yml"
"$preflight" --env "$tmp/complete.env" --compose "$tmp/restore-check.yml" \
    --source "$tmp/filled-source.yml" --restore-check --offline >/dev/null
expect_failure service-as-restore-check "the compose is not the --restore-check variant" \
    --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" --source "$tmp/filled-source.yml" \
    --restore-check

# Settings are checked in the rendered compose; a hand-edited value is also a stale render.
TOPUP_RPC_PROVIDER_B_URL=$TOPUP_RPC_PROVIDER_A_URL "$root/deploy/render-compose.sh" \
    "$tmp/filled-source.yml" >"$tmp/same-rpc.yml"
expect_failure same-rpc "the two RPC provider URLs must be different providers" \
    --env "$tmp/complete.env" --compose "$tmp/same-rpc.yml" --source "$tmp/filled-source.yml"
# A keyed provider is attested with {key} and its key sealed; a URL with the key itself is refused
# without printing it, and a key must fit its URL.
TOPUP_RPC_PROVIDER_A_URL=https://eth-sepolia.g.alchemy.com/v2/aB3dEfGhIjKlMnOpQrStUvWxYz012345 \
    "$root/deploy/render-compose.sh" "$tmp/filled-source.yml" >"$tmp/embedded-key.yml"
expect_failure embedded-key "TOPUP_RPC_PROVIDER_A_URL seems to embed an API key" \
    --env "$tmp/complete.env" --compose "$tmp/embedded-key.yml" --source "$tmp/filled-source.yml"
TOPUP_RPC_PROVIDER_A_URL='https://eth-sepolia.g.alchemy.com/v2/{key}' \
    "$root/deploy/render-compose.sh" "$tmp/filled-source.yml" >"$tmp/keyed.yml"
sed 's|^TOPUP_RPC_PROVIDER_A_KEY=.*|TOPUP_RPC_PROVIDER_A_KEY=sealed-key-0123456789|' \
    "$tmp/complete.env" >"$tmp/keyed.env"
sed 's|^TOPUP_RPC_PROVIDER_A_KEY=.*|TOPUP_RPC_PROVIDER_A_KEY=sealed/key|' "$tmp/complete.env" >"$tmp/bad-key.env"
"$preflight" --env "$tmp/keyed.env" --compose "$tmp/keyed.yml" --source "$tmp/filled-source.yml" \
    --offline >"$tmp/keyed.out"
"$preflight" --env "$tmp/complete.env" --compose "$tmp/keyed.yml" --source "$tmp/filled-source.yml" \
    --offline --unsealed >/dev/null
expect_failure missing-key "TOPUP_RPC_PROVIDER_A_KEY is required by the {key} placeholder" \
    --env "$tmp/complete.env" --compose "$tmp/keyed.yml" --source "$tmp/filled-source.yml"
expect_failure unused-key "TOPUP_RPC_PROVIDER_A_KEY is set, but TOPUP_RPC_PROVIDER_A_URL has no {key}" \
    --env "$tmp/keyed.env" --compose "$tmp/filled-route.yml" --source "$tmp/filled-source.yml"
expect_failure bad-key "TOPUP_RPC_PROVIDER_A_KEY must be at least 8 characters" \
    --env "$tmp/bad-key.env" --compose "$tmp/keyed.yml" --source "$tmp/filled-source.yml"
if grep -rqE 'aB3dEfGhIjKlMnOpQrStUvWxYz012345|sealed-key-0123456789|sealed/key' "$tmp"/*.out "$tmp"/*.err; then
    echo "preflight printed an RPC key" >&2
    exit 1
fi
sed 's|s3://topup-staging/postgres|s3://other/postgres|' "$tmp/filled-route.yml" >"$tmp/edited.yml"
expect_failure edited "differs from a fresh render" \
    --env "$tmp/complete.env" --compose "$tmp/edited.yml" --source "$tmp/filled-source.yml"
# The ingress must serve the domain topup verifies signatures against.
sed 's|DOMAIN: "pay-api-staging.phala.com"|DOMAIN: "other.phala.com"|' \
    "$tmp/filled-route.yml" >"$tmp/other-domain.yml"
expect_failure other-domain "dstack-ingress must serve TOPUP_DOMAIN" \
    --env "$tmp/complete.env" --compose "$tmp/other-domain.yml" --source "$tmp/filled-source.yml"

# Only the approved production image passes; no Phala Cloud node offers dstack 0.6.0.
for image in dstack-0.6.0-rc5 dstack-dev-0.5.9 dstack-nvidia-0.5.9 dstack-0.5.8; do
    expect_failure "image-$image" "OS image $image is not the approved dstack-0.5.9" \
        --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" \
        --source "$tmp/filled-source.yml" --os-image "$image"
done
"$preflight" --env "$tmp/complete.env" --compose "$tmp/filled-route.yml" \
    --source "$tmp/filled-source.yml" --os-image dstack-0.5.9 --offline >/dev/null

# A sealed Sentry DSN is accepted; a malformed one is refused without printing it.
sed -E 's|^SENTRY_DSN=.*|SENTRY_DSN=https://0123456789abcdef0123456789abcdef@o1.ingest.us.sentry.io/2|' \
    "$tmp/complete.env" >"$tmp/sentry.env"
"$preflight" --env "$tmp/sentry.env" --compose "$tmp/filled-route.yml" \
    --source "$tmp/filled-source.yml" --offline >/dev/null
sed -E 's|^SENTRY_DSN=.*|SENTRY_DSN=http://sentry-secret@example|' "$tmp/complete.env" >"$tmp/bad-sentry.env"
expect_failure bad-sentry "SENTRY_DSN must be empty or the project's DSN" \
    --env "$tmp/bad-sentry.env" --compose "$tmp/filled-route.yml" --source "$tmp/filled-source.yml"
if grep -q sentry-secret "$tmp/bad-sentry.out" "$tmp/bad-sentry.err"; then
    echo "preflight printed the SENTRY_DSN value" >&2
    exit 1
fi

# The CI-written env file has every owner-sealed secret empty: accepted only with --unsealed.
: >"$tmp/unsealed.env"
"$root/deploy/write-staging-env.sh" "$tmp/unsealed.env" >/dev/null
expect_failure unsealed "AWS_ACCESS_KEY_ID is empty" \
    --env "$tmp/unsealed.env" --compose "$tmp/filled-route.yml" --source "$tmp/filled-source.yml"
"$preflight" --env "$tmp/unsealed.env" --compose "$tmp/filled-route.yml" \
    --source "$tmp/filled-source.yml" --offline --unsealed >/dev/null

echo "preflight local checks test passed"
