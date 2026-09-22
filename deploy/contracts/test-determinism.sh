#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

require_command anvil
require_command cast
require_command forge
require_command jq
require_command ss

owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
treasury="$(cast compute-address "$owner" --nonce 1)"
admin="$treasury"
operator_role="$(cast keccak 'OPERATOR_ROLE')"
sample_salt="$(jq -er '.salts[0]' "$CONTRACTS_DIR/test-vectors/create2.json")"
expected="$DEPLOY_CONTRACTS_DIR/local-test-vectors.json"
[[ -f "$expected" ]] || die "missing local deterministic vectors: $expected"

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/crypto-topup-two-anvil.XXXXXX")"
pids=()
cleanup() {
    for pid in "${pids[@]}"; do
        kill "$pid" 2>/dev/null || true
        wait "$pid" 2>/dev/null || true
    done
    rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

ports=("$(find_free_port)" "")
first_port="${ports[0]}"
for candidate in $(seq $((first_port + 1)) 19646); do
    if ! ss -ltn "sport = :$candidate" 2>/dev/null | grep -q LISTEN; then
        ports[1]="$candidate"
        break
    fi
done
[[ -n "${ports[1]}" ]] || die "could not find a second free local port"

rpcs=("http://127.0.0.1:${ports[0]}" "http://127.0.0.1:${ports[1]}")
chain_ids=(31337 31338)
for index in 0 1; do
    anvil --silent --disable-default-create2-deployer \
        --port "${ports[$index]}" --chain-id "${chain_ids[$index]}" \
        >"$tmp_dir/anvil-$index.log" 2>&1 &
    pids+=("$!")
done
wait_for_rpc "${rpcs[0]}"
wait_for_rpc "${rpcs[1]}"

factory="$(predicted_factory "$admin" "$treasury")"
implementation="$(predicted_implementation "$factory")"
safe_expectations="$tmp_dir/safe-expectations.json"

reports='[]'
for index in 0 1; do
    rpc_url="${rpcs[$index]}"
    "$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$rpc_url" --local-fund --broadcast >/dev/null
    (
        cd "$CONTRACTS_DIR"
        SAFE_OWNER="$owner" forge script \
            test/DeployMockSafe.s.sol:DeployMockSafe \
            --rpc-url "$rpc_url" \
            --broadcast \
            --private-key "$ANVIL_PRIVATE_KEY" \
            -q
    ) >/dev/null
    [[ "$(cast code "$treasury" --rpc-url "$rpc_url")" != "0x" ]] || die "mock Safe was not deployed"

    if ((index == 0)); then
        jq -n \
            --arg treasury "$treasury" \
            --arg owner "$owner" \
            --arg code_hash "$(code_hash "$rpc_url" "$treasury")" \
            '{configured: true, treasury: $treasury, owners: [$owner], threshold: 1,
              proxy_code_hashes: [$code_hash]}' >"$safe_expectations"
        ADMIN="$admin" TREASURY="$treasury" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
            "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" \
            --rpc-url "$rpc_url" \
            --dry-run \
            --safe-expectations "$safe_expectations" >/dev/null 2>&1
        [[ "$(cast code "$factory" --rpc-url "$rpc_url")" == "0x" ]] || \
            die "dry-run wrote factory code to the target chain"
        ADMIN="$admin" TREASURY="$treasury" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
            "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" \
            --rpc-url "$rpc_url" \
            --broadcast \
            --safe-expectations "$safe_expectations" >/dev/null 2>&1
    else
        (
            cd "$CONTRACTS_DIR"
            ADMIN="$admin" TREASURY="$treasury" forge script \
                script/DeployFactory.s.sol:DeployFactory \
                --rpc-url "$rpc_url" \
                --broadcast \
                --private-key "$ANVIL_PRIVATE_KEY" \
                -q
        ) >/dev/null
    fi

    grant_calldata="$(cast calldata 'grantRole(bytes32,address)' "$operator_role" "$owner")"
    cast send "$treasury" 'exec(address,bytes)' "$factory" "$grant_calldata" \
        --rpc-url "$rpc_url" --private-key "$ANVIL_PRIVATE_KEY" >/dev/null
    cast send "$factory" 'flush(bytes32[],address)' "[$sample_salt]" \
        0x0000000000000000000000000000000000000000 \
        --rpc-url "$rpc_url" --private-key "$ANVIL_PRIVATE_KEY" >/dev/null

    actual_implementation="$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")"
    forwarder="$(cast call "$factory" 'addressOf(bytes32)(address)' "$sample_salt" --rpc-url "$rpc_url")"
    [[ "$(cast code "$forwarder" --rpc-url "$rpc_url")" != "0x" ]] || die "forwarder was not deployed"

    reports="$(jq -c \
        --argjson chain_id "${chain_ids[$index]}" \
        --arg factory "$factory" \
        --arg implementation "$actual_implementation" \
        --arg forwarder "$forwarder" \
        --arg factory_code_hash "$(code_hash "$rpc_url" "$factory")" \
        --arg implementation_code_hash "$(code_hash "$rpc_url" "$implementation")" \
        '. + [{chain_id: $chain_id, factory: $factory, implementation: $implementation,
               forwarder: $forwarder, factory_code_hash: $factory_code_hash,
               implementation_code_hash: $implementation_code_hash}]' <<<"$reports")"
done

[[ "$(jq -c '.[0] | del(.chain_id)' <<<"$reports")" == \
    "$(jq -c '.[1] | del(.chain_id)' <<<"$reports")" ]] || die "deployments differ across chain IDs"

actual="$(jq -c '.[0] | del(.chain_id)' <<<"$reports")"
committed="$(jq -c '{factory, implementation, forwarder, factory_code_hash, implementation_code_hash}' "$expected")"
[[ "$actual" == "$committed" ]] || die "local deterministic vectors drifted; inspect compiler or constructor input changes"

verification="$tmp_dir/verification.json"
ADMIN="$admin" TREASURY="$treasury" "$DEPLOY_CONTRACTS_DIR/verify-deployment.sh" \
    --safe-expectations "$safe_expectations" \
    --rpc "anvil-31337=${rpcs[0]}" \
    --rpc "anvil-31338=${rpcs[1]}" >"$verification"
jq -e '.passed == true' "$verification" >/dev/null

cast rpc --rpc-url "${rpcs[1]}" anvil_setCode "$factory" 0x60006000f3 >/dev/null
if (
    cd "$CONTRACTS_DIR"
    ADMIN="$admin" \
        TREASURY="$treasury" \
        EXPECTED_FACTORY_CODE_HASH="$(jq -er '.factory_code_hash' "$expected")" \
        EXPECTED_IMPLEMENTATION_CODE_HASH="$(jq -er '.implementation_code_hash' "$expected")" \
        forge script script/DeployFactory.s.sol:DeployFactory \
        --rpc-url "${rpcs[1]}" \
        --private-key "$ANVIL_PRIVATE_KEY" \
        -q
) >/dev/null 2>&1; then
    die "deployment script accepted an existing factory with the wrong code hash"
fi

jq -n \
    --argjson chains "$reports" \
    --slurpfile verification "$verification" \
    '{chains: $chains,
      verification: {
        safe: $verification[0].safe.passed,
        chains: [$verification[0].chains[] | {chain, passed}]
      },
      mismatched_code_rejected: true,
      passed: true}'
