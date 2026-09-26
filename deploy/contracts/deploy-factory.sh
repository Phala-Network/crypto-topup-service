#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

target=""
mode=""
safe_expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
while (($#)); do
    case "$1" in
        --rpc) target="${2:-}"; shift 2 ;;
        --dry-run) mode="dry-run"; shift ;;
        --broadcast) mode="broadcast"; shift ;;
        --safe-expectations) safe_expectations="${2:-}"; shift 2 ;;
        *) die "usage: $0 --rpc NETWORK[/LABEL]=URL (--dry-run|--broadcast) [--safe-expectations FILE]" ;;
    esac
done
[[ -n "$target" ]] || die "--rpc NETWORK[/LABEL]=URL is required"
[[ -n "$mode" ]] || die "one of --dry-run or --broadcast is required"
[[ -n "${PRIVATE_KEY:-}" ]] || die "PRIVATE_KEY must be set in the environment"

"$DEPLOY_CONTRACTS_DIR/check-build.sh" --check

safe_report="$(mktemp "${TMPDIR:-/tmp}/crypto-topup-safe-report.XXXXXX")"
tmp="$(mktemp "${TMPDIR:-/tmp}/crypto-topup-reference-manifest.XXXXXX")"
trap 'rm -f "$safe_report" "$tmp"' EXIT
validate_deployment_params "$safe_expectations" "$safe_report" "$target" || \
    die "refusing to deploy: ADMIN/TREASURY are not the verified approved Safes on $target"
parse_target "$safe_expectations" "$target"
rpc_url="$TARGET_RPC_URL"

"$DEPLOY_CONTRACTS_DIR/reference-manifest.sh" \
    --admin "$EXPECTED_ADMIN" \
    --treasury "$EXPECTED_TREASURY" \
    --output "$tmp" >/dev/null

EXPECTED_FACTORY_CODE_HASH="$(jq -er '.factory_code_hash' "$tmp")"
EXPECTED_IMPLEMENTATION_CODE_HASH="$(jq -er '.implementation_code_hash' "$tmp")"
export EXPECTED_FACTORY_CODE_HASH EXPECTED_IMPLEMENTATION_CODE_HASH

# DeployFactory.s.sol reads PRIVATE_KEY from the environment; it is never a forge argument.
export PRIVATE_KEY
args=(
    script/DeployFactory.s.sol:DeployFactory
    --rpc-url "$rpc_url"
)
if [[ "$mode" == "broadcast" ]]; then
    printf 'HUMAN-ONLY: broadcasting ForwarderFactory deployment\n' >&2
    args+=(--broadcast)
else
    printf 'dry-run: simulating only; no target-chain transaction will be broadcast\n' >&2
fi

(cd "$CONTRACTS_DIR" && forge script "${args[@]}")
