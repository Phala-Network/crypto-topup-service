#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

target=""
mode=""
networks="$DEPLOY_CONTRACTS_DIR/networks.json"
while (($#)); do
    case "$1" in
        --rpc) target="${2:-}"; shift 2 ;;
        --dry-run) mode="dry-run"; shift ;;
        --broadcast) mode="broadcast"; shift ;;
        --networks) networks="${2:-}"; shift 2 ;;
        *) die "usage: $0 --rpc NETWORK[/LABEL]=URL (--dry-run|--broadcast) [--networks FILE]" ;;
    esac
done
[[ -n "$target" ]] || die "--rpc NETWORK[/LABEL]=URL is required"
[[ -n "$mode" ]] || die "one of --dry-run or --broadcast is required"
[[ -n "${PRIVATE_KEY:-}" ]] || die "PRIVATE_KEY must be set in the environment"
require_command jq

"$DEPLOY_CONTRACTS_DIR/check-build.sh" --check

tmp="$(mktemp "${TMPDIR:-/tmp}/phala-pay-reference-manifest.XXXXXX")"
trap 'rm -f "$tmp"' EXIT
parse_target "$networks" "$target"
require_target_chain_id
rpc_url="$TARGET_RPC_URL"

"$DEPLOY_CONTRACTS_DIR/reference-manifest.sh" --output "$tmp" >/dev/null

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
