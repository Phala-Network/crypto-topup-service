#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

networks="$DEPLOY_CONTRACTS_DIR/networks.json"
rpcs=()
while (($#)); do
    case "$1" in
        --networks) networks="${2:-}"; shift 2 ;;
        --rpc) rpcs+=("${2:-}"); shift 2 ;;
        *) die "usage: $0 [--networks FILE] --rpc NETWORK[/LABEL]=URL [--rpc ...]" ;;
    esac
done
((${#rpcs[@]} > 0)) || die "at least one --rpc NETWORK[/LABEL]=URL is required"

require_command cast
require_command jq

"$DEPLOY_CONTRACTS_DIR/check-build.sh" --check >/dev/null

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/phala-pay-verification.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT

reference="$tmp_dir/reference.json"
"$DEPLOY_CONTRACTS_DIR/reference-manifest.sh" --output "$reference" >/dev/null

factory="$(jq -er '.factory' "$reference")"
implementation="$(jq -er '.implementation' "$reference")"
expected_proxy_hash="$(lower "$(jq -er '.proxy_code_hash' "$reference")")"
expected_factory_hash="$(lower "$(jq -er '.factory_code_hash' "$reference")")"
expected_implementation_hash="$(lower "$(jq -er '.implementation_code_hash' "$reference")")"
reports="$tmp_dir/reports.jsonl"
: >"$reports"
status=0

for entry in "${rpcs[@]}"; do
    parse_target "$networks" "$entry"
    rpc_url="$TARGET_RPC_URL"
    chain_id="$(rpc_chain_id "$rpc_url")"
    chain_id_ok=false
    [[ "$chain_id" == "$TARGET_EXPECTED_CHAIN_ID" ]] && chain_id_ok=true

    proxy_hash="$(lower "$(code_hash "$rpc_url" "$DETERMINISTIC_PROXY")")"
    factory_hash="$(lower "$(code_hash "$rpc_url" "$factory")")"
    implementation_hash="$(lower "$(code_hash "$rpc_url" "$implementation")")"

    proxy_ok=false
    factory_hash_ok=false
    implementation_hash_ok=false
    [[ "$proxy_hash" == "$expected_proxy_hash" ]] && proxy_ok=true
    [[ "$factory_hash" == "$expected_factory_hash" ]] && factory_hash_ok=true
    [[ "$implementation_hash" == "$expected_implementation_hash" ]] && implementation_hash_ok=true

    implementation_actual="error"
    factory_binding_actual="error"
    cast_call_ok=true
    implementation_actual="$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false
    factory_binding_actual="$(cast call "$implementation" 'factory()(address)' --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false

    implementation_ok=false
    factory_binding_ok=false
    [[ "$cast_call_ok" == true && "$(lower "$implementation_actual")" == "$(lower "$implementation")" ]] && implementation_ok=true
    [[ "$cast_call_ok" == true && "$(lower "$factory_binding_actual")" == "$(lower "$factory")" ]] && factory_binding_ok=true

    vector_reports='[]'
    vectors_ok=true
    while IFS=$'\t' read -r treasury salt expected_address; do
        actual_address="error"
        if ! actual_address="$(cast call "$factory" 'addressOf(address,bytes32)(address)' \
            "$treasury" "$salt" --rpc-url "$rpc_url" 2>/dev/null)"; then
            vectors_ok=false
        fi
        vector_ok=false
        if [[ "$(lower "$actual_address")" == "$(lower "$expected_address")" ]]; then
            vector_ok=true
        else
            vectors_ok=false
        fi
        vector_reports="$(jq -c \
            --arg treasury "$treasury" \
            --arg salt "$salt" \
            --arg expected "$expected_address" \
            --arg actual "$actual_address" \
            --argjson passed "$vector_ok" \
            '. + [{treasury: $treasury, salt: $salt, expected: $expected, actual: $actual,
                   passed: $passed}]' \
            <<<"$vector_reports")"
    done < <(jq -r '.sample_forwarders[] | [.treasury, .salt, .address] | @tsv' "$reference")

    passed=false
    if [[ "$chain_id_ok" == true && "$proxy_ok" == true && "$factory_hash_ok" == true && \
        "$implementation_hash_ok" == true && "$implementation_ok" == true && \
        "$factory_binding_ok" == true && "$vectors_ok" == true ]]; then
        passed=true
    else
        status=1
    fi

    jq -n \
        --arg target "$TARGET_NAME" \
        --arg network "$TARGET_NETWORK" \
        --argjson expected_chain_id "$TARGET_EXPECTED_CHAIN_ID" \
        --arg chain_id "$chain_id" \
        --argjson chain_id_ok "$chain_id_ok" \
        --arg factory "$factory" \
        --arg implementation "$implementation_actual" \
        --arg factory_code_hash "$factory_hash" \
        --arg implementation_code_hash "$implementation_hash" \
        --arg proxy_code_hash "$proxy_hash" \
        --argjson proxy_ok "$proxy_ok" \
        --argjson factory_hash_ok "$factory_hash_ok" \
        --argjson implementation_hash_ok "$implementation_hash_ok" \
        --argjson implementation_ok "$implementation_ok" \
        --argjson factory_binding_ok "$factory_binding_ok" \
        --argjson vectors_ok "$vectors_ok" \
        --argjson vectors "$vector_reports" \
        --argjson passed "$passed" \
        '{target: $target, network: $network, expected_chain_id: $expected_chain_id,
          chain_id: ($chain_id | tonumber? // $chain_id),
          factory: $factory, implementation: $implementation,
          code_hashes: {proxy: $proxy_code_hash, factory: $factory_code_hash,
                        implementation: $implementation_code_hash},
          checks: {chain_id: $chain_id_ok, proxy: $proxy_ok,
                   factory_code_hash: $factory_hash_ok,
                   implementation_code_hash: $implementation_hash_ok,
                   implementation: $implementation_ok,
                   implementation_factory: $factory_binding_ok,
                   sample_forwarders: $vectors_ok},
          vectors: $vectors, passed: $passed}' >>"$reports"
done

jq -n \
    --slurpfile chains "$reports" \
    --slurpfile reference "$reference" \
    '{reference: $reference[0], chains: $chains, passed: all($chains[]; .passed)}'
exit "$status"
