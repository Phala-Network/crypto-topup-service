#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
compose="$root/deploy/local/docker-compose.yml"
project="topup-smoke-$$"

cleanup() {
    docker compose -p "$project" -f "$compose" down --volumes --remove-orphans
}
trap cleanup EXIT INT TERM

wait_for() {
    description=$1
    shift
    attempts=60
    while [ "$attempts" -gt 0 ]; do
        if "$@" >/dev/null 2>&1; then
            return 0
        fi
        attempts=$((attempts - 1))
        sleep 2
    done
    echo "timed out waiting for $description" >&2
    return 1
}

export SOURCE_DATE_EPOCH=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}

docker compose -p "$project" -f "$compose" build postgres dstack-simulator topup
docker compose -p "$project" -f "$compose" up -d postgres dstack-simulator
wait_for postgres docker compose -p "$project" -f "$compose" exec -T postgres pg_isready -U postgres -d topup
wait_for dstack-simulator docker compose -p "$project" -f "$compose" exec -T dstack-simulator test -S /var/run/dstack.sock

docker compose -p "$project" -f "$compose" run --rm migrate
docker compose -p "$project" -f "$compose" run --rm --no-deps topup \
    topup route validate --template /etc/topup/routes/phala-cloud-sepolia-pha.yaml

attestation=$(docker compose -p "$project" -f "$compose" run --rm --no-deps topup \
    topup attest --nonce deadbeef)
printf '%s\n' "$attestation" | jq -e \
    '.keyid == "settlement/v1" and (.settlement_pubkey | length == 64) and (.quote | length > 0)' \
    >/dev/null
echo "dstack simulator attestation passed"

if rg -q 'healthz' "$root/crates/topup"; then
    docker compose -p "$project" -f "$compose" up -d topup
    wait_for /healthz curl -fsS "http://127.0.0.1:${TOPUP_LOCAL_PORT:-18080}/healthz"
    echo "/healthz passed"
else
    echo "skipped /healthz: C9 API is not present in this checkout"
fi

echo "local deployment smoke passed"
