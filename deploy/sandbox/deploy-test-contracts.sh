#!/usr/bin/env bash
# Deploys the sandbox-only contracts: the test token, a second token used by the
# unsupported-asset scenario, and MockSanctionsOracle. Prints a JSON manifest on stdout.
#
# HUMAN-ONLY on Sepolia: sign with a Foundry keystore account holding a funded deployer key
# (`--account NAME`; `forge` reads the keystore password from the file named by ETH_PASSWORD), so no key appears
# on a command line. `--anvil-unlocked ADDRESS` is for the local Anvil chain only. The forwarder
# factory is not deployed here; on Sepolia it comes from deploy/CONTRACTS.md.
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)
usage() {
    echo "usage: $0 --rpc-url URL (--account NAME | --anvil-unlocked ADDRESS)" >&2
    exit 2
}
rpc_url="" signer=()
while (($#)); do
    case "$1" in
        --rpc-url) rpc_url="${2:-}"; shift 2 ;;
        --account) signer=(--account "${2:?}"); shift 2 ;;
        --anvil-unlocked) signer=(--unlocked --from "${2:?}"); shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$rpc_url" && ${#signer[@]} -gt 0 ]] || usage
chain_id=$(cast chain-id --rpc-url "$rpc_url")
if [[ "${signer[0]}" == --unlocked ]] && [[ "$(cast client --rpc-url "$rpc_url")" != anvil* ]]; then
    echo "--anvil-unlocked is only for a local Anvil chain" >&2
    exit 2
fi

deploy() {
    (cd "$root/contracts" && forge create "$1" --rpc-url "$rpc_url" "${signer[@]}" \
        --broadcast --json) | jq -er '.deployedTo'
}

test_token=$(deploy test/mocks/MockTokens.sol:MockERC20)
unsupported_token=$(deploy test/mocks/MockTokens.sol:MockERC20)
sanctions_oracle=$(deploy test/mocks/MockSanctionsOracle.sol:MockSanctionsOracle)
jq -n \
    --argjson chain_id "$chain_id" \
    --arg test_token "$test_token" \
    --arg unsupported_token "$unsupported_token" \
    --arg sanctions_oracle "$sanctions_oracle" \
    '{chain_id: $chain_id, test_token: $test_token,
      unsupported_token: $unsupported_token, sanctions_oracle: $sanctions_oracle}'
