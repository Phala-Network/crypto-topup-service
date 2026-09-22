#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

rpc_url=""
mode=""
safe_expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
while (($#)); do
    case "$1" in
        --rpc-url) rpc_url="${2:-}"; shift 2 ;;
        --dry-run) mode="dry-run"; shift ;;
        --broadcast) mode="broadcast"; shift ;;
        --safe-expectations) safe_expectations="${2:-}"; shift 2 ;;
        *) die "usage: $0 --rpc-url URL (--dry-run|--broadcast) [--safe-expectations FILE]" ;;
    esac
done
[[ -n "$rpc_url" ]] || die "--rpc-url is required"
[[ -n "$mode" ]] || die "one of --dry-run or --broadcast is required"
[[ -n "${ADMIN:-}" && -n "${TREASURY:-}" && -n "${PRIVATE_KEY:-}" ]] || \
    die "ADMIN, TREASURY, and PRIVATE_KEY must be set in the environment"

"$DEPLOY_CONTRACTS_DIR/check-build.sh" --check

expected_treasury="$(jq -er '.treasury' "$safe_expectations")"
[[ "$(lower "$TREASURY")" == "$(lower "$expected_treasury")" ]] || \
    die "TREASURY does not match the committed Safe expectation"
"$DEPLOY_CONTRACTS_DIR/verify-safe.sh" \
    --expectations "$safe_expectations" \
    --rpc "target=$rpc_url" >/dev/null

tmp="$(mktemp "${TMPDIR:-/tmp}/crypto-topup-reference-manifest.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
"$DEPLOY_CONTRACTS_DIR/reference-manifest.sh" \
    --admin "$ADMIN" \
    --treasury "$TREASURY" \
    --output "$tmp" >/dev/null

export EXPECTED_FACTORY_CODE_HASH="$(jq -er '.factory_code_hash' "$tmp")"
export EXPECTED_IMPLEMENTATION_CODE_HASH="$(jq -er '.implementation_code_hash' "$tmp")"

args=(
    script/DeployFactory.s.sol:DeployFactory
    --rpc-url "$rpc_url"
    --private-key "$PRIVATE_KEY"
)
if [[ "$mode" == "broadcast" ]]; then
    printf 'HUMAN-ONLY: broadcasting ForwarderFactory deployment\n' >&2
    args+=(--broadcast)
else
    printf 'dry-run: simulating only; no target-chain transaction will be broadcast\n' >&2
fi

(cd "$CONTRACTS_DIR" && forge script "${args[@]}")
