#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

# The manifest of a local reference deployment of this build: the deterministic addresses, their
# runtime code hashes, and sample forwarder addresses. reference.json is the committed copy that
# verify-deployment.sh compares chains against, so verification needs no Solidity build; --check
# fails unless a fresh build reproduces it, and --write regenerates it after a reviewed change.
committed="$DEPLOY_CONTRACTS_DIR/reference.json"
output="" mode=""
while (($#)); do
    case "$1" in
        --output) output="${2:-}"; shift 2 ;;
        --check | --write) mode=$1; shift ;;
        *) die "usage: $0 [--output FILE | --check | --write]" ;;
    esac
done
[[ -z "$mode" || -z "$output" ]] || die "usage: $0 [--output FILE | --check | --write]"

require_command anvil
require_command cast
require_command forge
require_command jq

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/phala-pay-reference.XXXXXX")"
ANVIL_PID=""
cleanup() {
    if [[ -n "$ANVIL_PID" ]]; then
        kill "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
    fi
    rm -rf "$tmp_dir"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

start_anvil "$tmp_dir/anvil.log" --disable-default-create2-deployer --chain-id 31337
rpc_url="$ANVIL_RPC_URL"

"$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$rpc_url" --local-fund --broadcast >&2

factory="$(predicted_factory)"
implementation="$(predicted_implementation "$factory")"
init_code_hash="$(cast keccak "$(factory_init_code)")"

(
    cd "$CONTRACTS_DIR"
    FOUNDRY_BROADCAST="$tmp_dir/broadcast" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" forge script \
        script/DeployFactory.s.sol:DeployFactory \
        --rpc-url "$rpc_url" \
        --broadcast \
        -q
) >&2

actual_implementation="$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")"
[[ "$(lower "$actual_implementation")" == "$(lower "$implementation")" ]] || \
    die "reference deployment implementation mismatch"

# Sample forwarder addresses as this build's factory derives them, for the create2.json vectors.
vectors='[]'
while IFS=$'\t' read -r treasury salt; do
    address="$(cast call "$factory" 'addressOf(address,bytes32)(address)' "$treasury" "$salt" \
        --rpc-url "$rpc_url")"
    vectors="$(jq -c --arg treasury "$treasury" --arg salt "$salt" --arg address "$address" \
        '. + [{treasury: $treasury, salt: $salt, address: $address}]' <<<"$vectors")"
done < <(jq -r '.forwarders[] | [.treasury, .salt] | @tsv' "$CONTRACTS_DIR/test-vectors/create2.json")

manifest="$(jq -n \
    --arg proxy "$DETERMINISTIC_PROXY" \
    --arg proxy_code_hash "$(code_hash "$rpc_url" "$DETERMINISTIC_PROXY")" \
    --arg salt "$FACTORY_SALT" \
    --arg init_code_hash "$init_code_hash" \
    --arg factory "$factory" \
    --arg factory_code_hash "$(code_hash "$rpc_url" "$factory")" \
    --arg implementation "$implementation" \
    --arg implementation_code_hash "$(code_hash "$rpc_url" "$implementation")" \
    --argjson vectors "$vectors" \
    '{
        proxy: $proxy,
        proxy_code_hash: $proxy_code_hash,
        factory_salt: $salt,
        factory_init_code_hash: $init_code_hash,
        factory: $factory,
        factory_code_hash: $factory_code_hash,
        implementation: $implementation,
        implementation_code_hash: $implementation_code_hash,
        sample_forwarders: $vectors
    }')"

case "$mode" in
    --check)
        printf '%s\n' "$manifest" | jq . | cmp -s - "$committed" ||
            die "$committed differs from this build's reference deployment; review the contract change and run $0 --write"
        echo "reference.json matches this build's reference deployment" ;;
    --write) printf '%s\n' "$manifest" | jq . >"$committed" ;;
    *)
        if [[ -n "$output" ]]; then
            printf '%s\n' "$manifest" | jq . >"$output"
        else
            printf '%s\n' "$manifest" | jq .
        fi ;;
esac
