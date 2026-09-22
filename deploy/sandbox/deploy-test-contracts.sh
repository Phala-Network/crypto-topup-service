#!/usr/bin/env bash
# Deploys the sandbox-only contracts: the test token, a second token used by the
# unsupported-asset scenario, and MockSanctionsOracle. Prints a JSON manifest on stdout.
#
# HUMAN-ONLY on Sepolia: needs a funded deployer key in PRIVATE_KEY. The forwarder factory is
# not deployed here; on Sepolia it comes from deploy/CONTRACTS.md, locally from run-local.sh.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
rpc_url=""
while (($#)); do
    case "$1" in
        --rpc-url) rpc_url="${2:-}"; shift 2 ;;
        *) echo "usage: $0 --rpc-url URL" >&2; exit 2 ;;
    esac
done
[[ -n "$rpc_url" ]] || { echo "--rpc-url is required" >&2; exit 2; }
[[ -n "${PRIVATE_KEY:-}" ]] || { echo "PRIVATE_KEY must be set in the environment" >&2; exit 2; }

deploy() {
    (cd "$root/contracts" && forge create "$1" --rpc-url "$rpc_url" --private-key "$PRIVATE_KEY" \
        --broadcast --json) | jq -er '.deployedTo'
}

test_token=$(deploy test/mocks/MockTokens.sol:MockERC20)
unsupported_token=$(deploy test/mocks/MockTokens.sol:MockERC20)
sanctions_oracle=$(deploy test/mocks/MockSanctionsOracle.sol:MockSanctionsOracle)
jq -n \
    --arg chain_id "$(cast chain-id --rpc-url "$rpc_url")" \
    --arg test_token "$test_token" \
    --arg unsupported_token "$unsupported_token" \
    --arg sanctions_oracle "$sanctions_oracle" \
    '{chain_id: ($chain_id | tonumber), test_token: $test_token,
      unsupported_token: $unsupported_token, sanctions_oracle: $sanctions_oracle}'
