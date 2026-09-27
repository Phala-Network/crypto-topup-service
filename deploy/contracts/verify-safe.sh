#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
rpcs=()
while (($#)); do
    case "$1" in
        --expectations) expectations="${2:-}"; shift 2 ;;
        --rpc) rpcs+=("${2:-}"); shift 2 ;;
        *) die "usage: $0 [--expectations FILE] --rpc NETWORK[/LABEL]=URL [--rpc ...]" ;;
    esac
done
((${#rpcs[@]} > 0)) || die "at least one --rpc NETWORK[/LABEL]=URL is required"

require_command cast
require_command jq

# Safe v1.4.1 constants: keccak256("guard_manager.guard.address"),
# keccak256("fallback_manager.handler.address"), and the module list sentinel.
GUARD_STORAGE_SLOT="0x4a204f620c8c5ccdca3fd54d003badd85ba500436a431f0cbda4f558c93c34c8"
FALLBACK_HANDLER_STORAGE_SLOT="0x6c9a6c4a39284e37ed1cf53d337577d14212a4870fb976a4366c693b939918d5"
SENTINEL_MODULES="0x0000000000000000000000000000000000000001"
MODULES_PAGE_SIZE=100

load_expectations "$expectations"

# Verify each approved Safe once, reporting every role it fills.
mapfile -t safes_to_check < <(jq -c '
    [{role: "admin", address: .admin}, {role: "treasury", address: .treasury}]
    | group_by(.address | ascii_downcase)
    | .[] | {address: .[0].address, roles: map(.role)}
' "$expectations")

reports="$(mktemp "${TMPDIR:-/tmp}/phala-pay-safe-report.XXXXXX")"
trap 'rm -f "$reports"' EXIT
status=0

fail() {
    printf 'error: %s: %s\n' "$TARGET_NAME" "$*" >&2
}

for entry in "${rpcs[@]}"; do
    parse_target "$expectations" "$entry"
    chain_id="$(rpc_chain_id "$TARGET_RPC_URL")"
    chain_id_ok=false
    if [[ "$chain_id" == "$TARGET_EXPECTED_CHAIN_ID" ]]; then
        chain_id_ok=true
    else
        fail "network $TARGET_NETWORK expects chain id $TARGET_EXPECTED_CHAIN_ID but the RPC reports $chain_id"
    fi

    safe_reports='[]'
    for safe_to_check in "${safes_to_check[@]}"; do
        address="$(jq -r '.address' <<<"$safe_to_check")"
        roles="$(jq -c '.roles' <<<"$safe_to_check")"
        expected="$(jq -c --arg address "$address" \
            '.safes[] | select((.address | ascii_downcase) == ($address | ascii_downcase))' \
            "$expectations")"
        expected_singleton="$(lower "$(jq -r '.singleton' <<<"$expected")")"
        expected_singleton_code_hash="$(lower "$(jq -r '.singleton_code_hash' <<<"$expected")")"
        expected_threshold="$(jq -r '.threshold' <<<"$expected")"
        expected_owners="$(jq -c '.owners | map(ascii_downcase) | sort' <<<"$expected")"

        actual_code_hash="$(lower "$(code_hash "$TARGET_RPC_URL" "$address")")"
        contract_ok=false
        [[ "$actual_code_hash" != "$ZERO_HASH" ]] && contract_ok=true
        code_hash_ok="$(jq --arg hash "$actual_code_hash" \
            '.proxy_code_hashes | map(ascii_downcase) | index($hash) != null' <<<"$expected")"

        # SafeProxy keeps the singleton in slot 0 and answers masterCopy() without delegating.
        # The proxy code hash is pinned above, so slot 0 is the implementation that will run.
        singleton_actual="error"
        if slot="$(cast storage "$address" 0 --rpc-url "$TARGET_RPC_URL" 2>/dev/null)"; then
            singleton_actual="0x${slot: -40}"
        fi
        master_copy_actual="$(cast call "$address" 'masterCopy()(address)' \
            --rpc-url "$TARGET_RPC_URL" 2>/dev/null)" || master_copy_actual="error"
        singleton_code_hash="$(lower "$(code_hash "$TARGET_RPC_URL" "$expected_singleton")")"
        singleton_ok=false
        master_copy_ok=false
        singleton_code_hash_ok=false
        [[ "$(lower "$singleton_actual")" == "$expected_singleton" ]] && singleton_ok=true
        [[ "$(lower "$master_copy_actual")" == "$expected_singleton" ]] && master_copy_ok=true
        [[ "$singleton_code_hash" == "$expected_singleton_code_hash" ]] && singleton_code_hash_ok=true

        # Safe v1.4.1 keeps the guard and fallback handler in fixed slots and the modules in a
        # sentinel-terminated list. Any of them can move funds or veto transactions without the
        # owners, so each must match the approved set exactly (zero address for none).
        expected_modules="$(jq -c '.modules | map(ascii_downcase) | sort' <<<"$expected")"
        expected_guard="$(lower "$(jq -r '.guard' <<<"$expected")")"
        expected_fallback_handler="$(lower "$(jq -r '.fallback_handler' <<<"$expected")")"
        modules_json='"error"'
        modules_ok=false
        modules_error=""
        if modules_call="$(cast call "$address" 'getModulesPaginated(address,uint256)(address[],address)' \
            "$SENTINEL_MODULES" "$MODULES_PAGE_SIZE" --json --rpc-url "$TARGET_RPC_URL" 2>/dev/null)"; then
            modules_json="$(jq -c '.[0] | map(ascii_downcase) | sort' <<<"$modules_call")"
            # A next pointer other than the sentinel means the list did not fit in one page.
            if [[ "$(lower "$(jq -r '.[1]' <<<"$modules_call")")" != "$SENTINEL_MODULES" ]]; then
                modules_error="module list did not end at the sentinel (more than $MODULES_PAGE_SIZE modules)"
            elif [[ "$modules_json" != "$expected_modules" ]]; then
                modules_error="modules $modules_json do not match $expected_modules"
            fi
        else
            modules_error="getModulesPaginated failed; the enabled modules cannot be read"
        fi
        [[ -n "$modules_error" ]] || modules_ok=true
        guard_actual="error"
        if slot="$(cast storage "$address" "$GUARD_STORAGE_SLOT" --rpc-url "$TARGET_RPC_URL" 2>/dev/null)"; then
            guard_actual="$(lower "0x${slot: -40}")"
        fi
        fallback_handler_actual="error"
        if slot="$(cast storage "$address" "$FALLBACK_HANDLER_STORAGE_SLOT" \
            --rpc-url "$TARGET_RPC_URL" 2>/dev/null)"; then
            fallback_handler_actual="$(lower "0x${slot: -40}")"
        fi
        guard_ok=false
        fallback_handler_ok=false
        [[ "$guard_actual" == "$expected_guard" ]] && guard_ok=true
        [[ "$fallback_handler_actual" == "$expected_fallback_handler" ]] && fallback_handler_ok=true

        owners_json='[]'
        threshold_actual="unknown"
        owners_ok=false
        threshold_ok=false
        if owners_call="$(cast call "$address" 'getOwners()(address[])' --json \
            --rpc-url "$TARGET_RPC_URL" 2>/dev/null)" &&
            threshold_call="$(cast call "$address" 'getThreshold()(uint256)' --json \
                --rpc-url "$TARGET_RPC_URL" 2>/dev/null)"; then
            owners_json="$(jq -c '.[0] | map(ascii_downcase) | sort' <<<"$owners_call")"
            threshold_actual="$(jq -r '.[0]' <<<"$threshold_call")"
            [[ "$owners_json" == "$expected_owners" ]] && owners_ok=true
            [[ "$threshold_actual" == "$expected_threshold" ]] && threshold_ok=true
        fi

        [[ "$contract_ok" == true ]] || \
            fail "$(jq -r 'join("/")' <<<"$roles") $address has no code (EOA or undeployed); it must be the approved Safe"
        [[ "$code_hash_ok" == true ]] || fail "$address proxy code hash $actual_code_hash is not approved"
        [[ "$singleton_ok" == true ]] || \
            fail "$address singleton (slot 0) is $singleton_actual, expected $expected_singleton"
        [[ "$master_copy_ok" == true ]] || \
            fail "$address masterCopy() is $master_copy_actual, expected $expected_singleton"
        [[ "$singleton_code_hash_ok" == true ]] || \
            fail "singleton $expected_singleton code hash $singleton_code_hash is not approved"
        [[ "$owners_ok" == true ]] || fail "$address owners $owners_json do not match $expected_owners"
        [[ "$threshold_ok" == true ]] || \
            fail "$address threshold $threshold_actual does not match $expected_threshold"
        [[ "$modules_ok" == true ]] || fail "$address $modules_error"
        [[ "$guard_ok" == true ]] || fail "$address guard $guard_actual does not match $expected_guard"
        [[ "$fallback_handler_ok" == true ]] || \
            fail "$address fallback handler $fallback_handler_actual does not match $expected_fallback_handler"

        safe_reports="$(jq -c \
            --arg address "$address" \
            --argjson roles "$roles" \
            --arg code_hash "$actual_code_hash" \
            --arg singleton "$singleton_actual" \
            --arg master_copy "$master_copy_actual" \
            --arg singleton_code_hash "$singleton_code_hash" \
            --argjson owners "$owners_json" \
            --arg threshold "$threshold_actual" \
            --argjson modules "$modules_json" \
            --arg guard "$guard_actual" \
            --arg fallback_handler "$fallback_handler_actual" \
            --argjson contract_ok "$contract_ok" \
            --argjson code_hash_ok "$code_hash_ok" \
            --argjson singleton_ok "$singleton_ok" \
            --argjson master_copy_ok "$master_copy_ok" \
            --argjson singleton_code_hash_ok "$singleton_code_hash_ok" \
            --argjson owners_ok "$owners_ok" \
            --argjson threshold_ok "$threshold_ok" \
            --argjson modules_ok "$modules_ok" \
            --argjson guard_ok "$guard_ok" \
            --argjson fallback_handler_ok "$fallback_handler_ok" \
            '. + [{address: $address, roles: $roles, code_hash: $code_hash,
                   singleton: $singleton, master_copy: $master_copy,
                   singleton_code_hash: $singleton_code_hash,
                   owners: $owners, threshold: $threshold, modules: $modules,
                   guard: $guard, fallback_handler: $fallback_handler,
                   checks: {contract: $contract_ok, safe_proxy_code_hash: $code_hash_ok,
                            singleton: $singleton_ok, master_copy: $master_copy_ok,
                            singleton_code_hash: $singleton_code_hash_ok,
                            owners: $owners_ok, threshold: $threshold_ok,
                            modules: $modules_ok, guard: $guard_ok,
                            fallback_handler: $fallback_handler_ok}}
                  | .passed = all(.checks[]; .)]' <<<"$safe_reports")"
    done

    passed="$(jq --argjson chain_id_ok "$chain_id_ok" \
        '$chain_id_ok and all(.[]; .passed)' <<<"$safe_reports")"
    [[ "$passed" == true ]] || status=1

    jq -n \
        --arg target "$TARGET_NAME" \
        --arg network "$TARGET_NETWORK" \
        --argjson expected_chain_id "$TARGET_EXPECTED_CHAIN_ID" \
        --arg chain_id "$chain_id" \
        --argjson chain_id_ok "$chain_id_ok" \
        --argjson safes "$safe_reports" \
        --argjson passed "$passed" \
        '{target: $target, network: $network, expected_chain_id: $expected_chain_id,
          chain_id: ($chain_id | tonumber? // $chain_id),
          checks: {chain_id: $chain_id_ok}, safes: $safes, passed: $passed}' >>"$reports"
done

jq -s --arg expectations "$expectations" \
    '{expectations: $expectations, chains: ., passed: all(.[]; .passed)}' "$reports"
exit "$status"
