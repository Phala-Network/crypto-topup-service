#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

safe_expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
rpcs=()
while (($#)); do
    case "$1" in
        --safe-expectations) safe_expectations="${2:-}"; shift 2 ;;
        --rpc) rpcs+=("${2:-}"); shift 2 ;;
        *) die "usage: $0 [--safe-expectations FILE] --rpc NAME=URL [--rpc NAME=URL ...]" ;;
    esac
done
((${#rpcs[@]} > 0)) || die "at least one --rpc NAME=URL is required"
[[ -n "${ADMIN:-}" && -n "${TREASURY:-}" ]] || die "ADMIN and TREASURY must be set"

require_command cast
require_command jq

"$DEPLOY_CONTRACTS_DIR/check-build.sh" --check >/dev/null

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/crypto-topup-verification.XXXXXX")"
trap 'rm -rf "$tmp_dir"' EXIT
safe_args=(--expectations "$safe_expectations")
for entry in "${rpcs[@]}"; do
    safe_args+=(--rpc "$entry")
done
safe_status=0
"$DEPLOY_CONTRACTS_DIR/verify-safe.sh" "${safe_args[@]}" >"$tmp_dir/safe.json" || safe_status=$?
if ((safe_status != 0)); then
    jq -n --slurpfile safe "$tmp_dir/safe.json" \
        '{safe: ($safe[0] // null), chains: [], passed: false}'
    exit 1
fi

reference="$tmp_dir/reference.json"
"$DEPLOY_CONTRACTS_DIR/reference-manifest.sh" \
    --admin "$ADMIN" \
    --treasury "$TREASURY" \
    --output "$reference" >/dev/null

factory="$(jq -er '.factory' "$reference")"
implementation="$(jq -er '.implementation' "$reference")"
expected_proxy_hash="$(lower "$(jq -er '.proxy_code_hash' "$reference")")"
expected_factory_hash="$(lower "$(jq -er '.factory_code_hash' "$reference")")"
expected_implementation_hash="$(lower "$(jq -er '.implementation_code_hash' "$reference")")"
reports="$tmp_dir/reports.jsonl"
status=0

for entry in "${rpcs[@]}"; do
    [[ "$entry" == *=* ]] || die "RPC must be NAME=URL: $entry"
    name="${entry%%=*}"
    rpc_url="${entry#*=}"

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
    treasury_actual="error"
    factory_binding_actual="error"
    admin_role_actual="error"
    cast_call_ok=true
    implementation_actual="$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false
    treasury_actual="$(cast call "$implementation" 'treasury()(address)' --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false
    factory_binding_actual="$(cast call "$implementation" 'factory()(address)' --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false
    admin_role_actual="$(cast call "$factory" 'hasRole(bytes32,address)(bool)' \
        0x0000000000000000000000000000000000000000000000000000000000000000 \
        "$ADMIN" --rpc-url "$rpc_url" 2>/dev/null)" || cast_call_ok=false

    implementation_ok=false
    treasury_ok=false
    factory_binding_ok=false
    admin_role_ok=false
    [[ "$cast_call_ok" == true && "$(lower "$implementation_actual")" == "$(lower "$implementation")" ]] && implementation_ok=true
    [[ "$cast_call_ok" == true && "$(lower "$treasury_actual")" == "$(lower "$TREASURY")" ]] && treasury_ok=true
    [[ "$cast_call_ok" == true && "$(lower "$factory_binding_actual")" == "$(lower "$factory")" ]] && factory_binding_ok=true
    [[ "$cast_call_ok" == true && "$admin_role_actual" == "true" ]] && admin_role_ok=true

    vector_reports='[]'
    vectors_ok=true
    while IFS=$'\t' read -r salt expected_address; do
        actual_address="error"
        if ! actual_address="$(cast call "$factory" 'addressOf(bytes32)(address)' \
            "$salt" --rpc-url "$rpc_url" 2>/dev/null)"; then
            vectors_ok=false
        fi
        vector_ok=false
        if [[ "$(lower "$actual_address")" == "$(lower "$expected_address")" ]]; then
            vector_ok=true
        else
            vectors_ok=false
        fi
        vector_reports="$(jq -c \
            --arg salt "$salt" \
            --arg expected "$expected_address" \
            --arg actual "$actual_address" \
            --argjson passed "$vector_ok" \
            '. + [{salt: $salt, expected: $expected, actual: $actual, passed: $passed}]' \
            <<<"$vector_reports")"
    done < <(jq -r '.sample_forwarders[] | [.salt, .address] | @tsv' "$reference")

    passed=false
    if [[ "$proxy_ok" == true && "$factory_hash_ok" == true && \
        "$implementation_hash_ok" == true && "$implementation_ok" == true && \
        "$treasury_ok" == true && "$factory_binding_ok" == true && \
        "$admin_role_ok" == true && "$vectors_ok" == true ]]; then
        passed=true
    else
        status=1
    fi

    jq -n \
        --arg chain "$name" \
        --arg factory "$factory" \
        --arg implementation "$implementation_actual" \
        --arg treasury "$treasury_actual" \
        --arg factory_code_hash "$factory_hash" \
        --arg implementation_code_hash "$implementation_hash" \
        --arg proxy_code_hash "$proxy_hash" \
        --argjson proxy_ok "$proxy_ok" \
        --argjson factory_hash_ok "$factory_hash_ok" \
        --argjson implementation_hash_ok "$implementation_hash_ok" \
        --argjson implementation_ok "$implementation_ok" \
        --argjson treasury_ok "$treasury_ok" \
        --argjson factory_binding_ok "$factory_binding_ok" \
        --argjson admin_role_ok "$admin_role_ok" \
        --argjson vectors_ok "$vectors_ok" \
        --argjson vectors "$vector_reports" \
        --argjson passed "$passed" \
        '{chain: $chain, factory: $factory, implementation: $implementation, treasury: $treasury,
          code_hashes: {proxy: $proxy_code_hash, factory: $factory_code_hash,
                        implementation: $implementation_code_hash},
          checks: {proxy: $proxy_ok, factory_code_hash: $factory_hash_ok,
                   implementation_code_hash: $implementation_hash_ok,
                   implementation: $implementation_ok, treasury: $treasury_ok,
                   implementation_factory: $factory_binding_ok,
                   default_admin_role: $admin_role_ok, sample_forwarders: $vectors_ok},
          vectors: $vectors, passed: $passed}' >>"$reports"
done

jq -n \
    --slurpfile safe "$tmp_dir/safe.json" \
    --slurpfile chains "$reports" \
    --slurpfile reference "$reference" \
    '{reference: $reference[0], safe: $safe[0], chains: $chains,
      passed: all($chains[]; .passed)}'
exit "$status"
