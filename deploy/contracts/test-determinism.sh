#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

require_command anvil
require_command cast
require_command forge
require_command jq

owner="0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266"
safe_singleton="$(cast compute-address "$owner" --nonce 0)"
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
    # An anvil that start_anvil spawned but has not returned yet is not in pids.
    if [[ -n "${ANVIL_PID:-}" ]]; then
        kill "$ANVIL_PID" 2>/dev/null || true
        wait "$ANVIL_PID" 2>/dev/null || true
    fi
    rm -rf "$tmp_dir"
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

rpcs=()
chain_ids=(31337 31338)
for index in 0 1; do
    start_anvil "$tmp_dir/anvil-$index.log" --disable-default-create2-deployer \
        --chain-id "${chain_ids[$index]}"
    pids+=("$ANVIL_PID")
    rpcs+=("$ANVIL_RPC_URL")
done

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
            --arg singleton "$safe_singleton" \
            --arg singleton_code_hash "$(code_hash "$rpc_url" "$safe_singleton")" \
            --arg zero "$ZERO_ADDRESS" \
            '{configured: true,
              networks: {"anvil-31337": {chain_id: 31337}, "anvil-31338": {chain_id: 31338}},
              admin: $treasury, treasury: $treasury,
              safes: [{address: $treasury, owners: [$owner], threshold: 1,
                       proxy_code_hashes: [$code_hash],
                       singleton: $singleton, singleton_code_hash: $singleton_code_hash,
                       modules: [], guard: $zero, fallback_handler: $zero}]}' \
            >"$safe_expectations"
        ADMIN="$admin" TREASURY="$treasury" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
            "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" \
            --rpc "anvil-31337=$rpc_url" \
            --dry-run \
            --safe-expectations "$safe_expectations" >/dev/null 2>&1
        [[ "$(cast code "$factory" --rpc-url "$rpc_url")" == "0x" ]] || \
            die "dry-run wrote factory code to the target chain"
        ADMIN="$admin" TREASURY="$treasury" PRIVATE_KEY="$ANVIL_PRIVATE_KEY" \
            "$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" \
            --rpc "anvil-31337=$rpc_url" \
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
jq -e '[.chains[], .safe.chains[]] | length == 4 and
    all(.chain_id == .expected_chain_id and .checks.chain_id)' "$verification" >/dev/null || \
    die "verification report does not bind every target to its expected chain id"

rejected='[]'
# Runs a command that must fail and keeps its stdout/stderr for the reason assertions below.
expect_rejection() {
    local name="$1"
    shift
    if "$@" >"$tmp_dir/$name.out" 2>"$tmp_dir/$name.err"; then
        die "$name: command was accepted"
    fi
    rejected="$(jq -c --arg name "$name" '. + [$name]' <<<"$rejected")"
}
require_error() {
    grep -qF -- "$2" "$tmp_dir/$1.err" || \
        die "$1: rejected for an unexpected reason: $(cat "$tmp_dir/$1.err")"
}
require_report() {
    local name="$1"
    shift
    jq -e "$@" "$tmp_dir/$name.out" >/dev/null || \
        die "$name: unexpected report: $(cat "$tmp_dir/$name.out")"
}
deploy_factory=("$DEPLOY_CONTRACTS_DIR/deploy-factory.sh" --dry-run)
verify_deployment=("$DEPLOY_CONTRACTS_DIR/verify-deployment.sh")

# A target whose RPC is on another network is rejected, and the report shows both chain ids.
expect_rejection wrong_network_verify env ADMIN="$admin" TREASURY="$treasury" \
    "${verify_deployment[@]}" --safe-expectations "$safe_expectations" \
    --rpc "anvil-31337=${rpcs[1]}"
require_report wrong_network_verify '.passed == false and (.safe.chains[0] |
    .network == "anvil-31337" and .expected_chain_id == 31337 and .chain_id == 31338 and
    .checks.chain_id == false and all(.safes[]; .passed))'
expect_rejection wrong_network_deploy env ADMIN="$admin" TREASURY="$treasury" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "${deploy_factory[@]}" \
    --safe-expectations "$safe_expectations" --rpc "anvil-31338=${rpcs[0]}"
require_error wrong_network_deploy "expects chain id 31338 but the RPC reports 31337"

# ADMIN and TREASURY must be exactly the approved Safes from the expectations file.
expect_rejection treasury_mismatch_verify env ADMIN="$admin" TREASURY="$owner" \
    "${verify_deployment[@]}" --safe-expectations "$safe_expectations" \
    --rpc "anvil-31337=${rpcs[0]}"
require_error treasury_mismatch_verify "does not match the approved treasury Safe"
expect_rejection treasury_mismatch_deploy env ADMIN="$admin" TREASURY="$owner" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "${deploy_factory[@]}" \
    --safe-expectations "$safe_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_error treasury_mismatch_deploy "does not match the approved treasury Safe"
expect_rejection admin_mismatch_deploy env ADMIN="$owner" TREASURY="$treasury" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "${deploy_factory[@]}" \
    --safe-expectations "$safe_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_error admin_mismatch_deploy "does not match the approved admin Safe"

# An EOA is never accepted as the admin, even if the expectations file lists it as a Safe.
eoa_expectations="$tmp_dir/eoa-admin-expectations.json"
jq --arg owner "$owner" '.admin = $owner | .safes += [.safes[0] | .address = $owner]' \
    "$safe_expectations" >"$eoa_expectations"
expect_rejection eoa_admin_deploy env ADMIN="$owner" TREASURY="$treasury" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "${deploy_factory[@]}" \
    --safe-expectations "$eoa_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_error eoa_admin_deploy "admin $owner has no code (EOA or undeployed)"

# A module, guard, or fallback handler the expectations do not list is rejected; listing them
# approves them. The changes go through the Safe itself and are rolled back afterwards.
snapshot="$(cast rpc --rpc-url "${rpcs[0]}" evm_snapshot | tr -d '"')"
safe_module="0x1111111111111111111111111111111111111111"
safe_guard="0x2222222222222222222222222222222222222222"
safe_fallback_handler="0x3333333333333333333333333333333333333333"
safe_self_call() {
    cast send "$treasury" 'exec(address,bytes)' "$treasury" "$(cast calldata "$1" "$2")" \
        --rpc-url "${rpcs[0]}" --private-key "$ANVIL_PRIVATE_KEY" >/dev/null
}
safe_self_call 'enableModule(address)' "$safe_module"
safe_self_call 'setGuard(address)' "$safe_guard"
safe_self_call 'setFallbackHandler(address)' "$safe_fallback_handler"
expect_rejection safe_drift_verify "$DEPLOY_CONTRACTS_DIR/verify-safe.sh" \
    --expectations "$safe_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_report safe_drift_verify --arg module "$safe_module" --arg guard "$safe_guard" \
    --arg handler "$safe_fallback_handler" '.chains[0].safes[0] |
    .modules == [$module] and .guard == $guard and .fallback_handler == $handler and
    (.checks | (.modules | not) and (.guard | not) and (.fallback_handler | not) and
    (del(.modules, .guard, .fallback_handler) | all))'
require_error safe_drift_verify "guard $safe_guard does not match $ZERO_ADDRESS"
approved_drift_expectations="$tmp_dir/approved-drift-expectations.json"
jq --arg module "$safe_module" --arg guard "$safe_guard" --arg handler "$safe_fallback_handler" \
    '.safes[0] += {modules: [$module], guard: $guard, fallback_handler: $handler}' \
    "$safe_expectations" >"$approved_drift_expectations"
"$DEPLOY_CONTRACTS_DIR/verify-safe.sh" --expectations "$approved_drift_expectations" \
    --rpc "anvil-31337=${rpcs[0]}" >/dev/null || die "approved modules, guard, and fallback handler were rejected"
# Clearing the module list head (MockSafeBase `modules` is slot 4) makes getModulesPaginated
# revert, as on an uninitialized Safe v1.4.1; an unreadable module list is rejected.
cast rpc --rpc-url "${rpcs[0]}" anvil_setStorageAt "$treasury" \
    "$(cast index address 0x0000000000000000000000000000000000000001 4)" "$ZERO_HASH" >/dev/null
expect_rejection unreadable_modules_verify "$DEPLOY_CONTRACTS_DIR/verify-safe.sh" \
    --expectations "$approved_drift_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_error unreadable_modules_verify "getModulesPaginated failed"
cast rpc --rpc-url "${rpcs[0]}" evm_revert "$snapshot" >/dev/null

# The same proxy code, owners, and threshold in front of another singleton is rejected.
nonce="$(cast nonce "$owner" --rpc-url "${rpcs[0]}")"
malicious_singleton="$(cast compute-address "$owner" --nonce "$nonce")"
malicious_safe="$(cast compute-address "$owner" --nonce $((nonce + 1)))"
(
    cd "$CONTRACTS_DIR"
    SAFE_OWNER="$owner" forge script \
        test/DeployMockSafe.s.sol:DeployMockSafe \
        --sig 'runMalicious()' \
        --rpc-url "${rpcs[0]}" \
        --broadcast \
        --private-key "$ANVIL_PRIVATE_KEY" \
        -q
) >/dev/null
[[ "$(code_hash "${rpcs[0]}" "$malicious_safe")" == "$(code_hash "${rpcs[0]}" "$treasury")" ]] || \
    die "malicious Safe proxy code differs from the approved proxy code"
malicious_expectations="$tmp_dir/malicious-singleton-expectations.json"
jq --arg safe "$malicious_safe" '.admin = $safe | .treasury = $safe | .safes[0].address = $safe' \
    "$safe_expectations" >"$malicious_expectations"
expect_rejection wrong_singleton_verify "$DEPLOY_CONTRACTS_DIR/verify-safe.sh" \
    --expectations "$malicious_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_report wrong_singleton_verify --arg singleton "$(lower "$malicious_singleton")" \
    '.chains[0].safes[0] | .singleton == $singleton and
    (.master_copy | ascii_downcase) == $singleton and
    (.checks | .contract and .safe_proxy_code_hash and .owners and .threshold and
    .singleton_code_hash and (.singleton | not) and (.master_copy | not))'
expect_rejection wrong_singleton_deploy env ADMIN="$malicious_safe" TREASURY="$malicious_safe" \
    PRIVATE_KEY="$ANVIL_PRIVATE_KEY" "${deploy_factory[@]}" \
    --safe-expectations "$malicious_expectations" --rpc "anvil-31337=${rpcs[0]}"
require_error wrong_singleton_deploy "singleton (slot 0) is"

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

# The approved singleton address with different code on one chain is rejected on that chain.
cast rpc --rpc-url "${rpcs[1]}" anvil_setCode "$safe_singleton" \
    "$(cast code "$malicious_singleton" --rpc-url "${rpcs[0]}")" >/dev/null
expect_rejection singleton_code_verify "$DEPLOY_CONTRACTS_DIR/verify-safe.sh" \
    --expectations "$safe_expectations" \
    --rpc "anvil-31337=${rpcs[0]}" --rpc "anvil-31338=${rpcs[1]}"
require_report singleton_code_verify '.chains[0].passed and
    (.chains[1].safes[0].checks | (.singleton_code_hash | not) and
    (del(.singleton_code_hash) | all))'

jq -n \
    --argjson chains "$reports" \
    --slurpfile verification "$verification" \
    --argjson rejected "$rejected" \
    '{chains: $chains,
      verification: {
        safe: $verification[0].safe.passed,
        chains: [$verification[0].chains[] | {target, expected_chain_id, chain_id, passed}]
      },
      mismatched_code_rejected: true,
      rejected: $rejected,
      passed: true}'
