#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

expectations="$DEPLOY_CONTRACTS_DIR/safe-expectations.json"
rpcs=()
while (($#)); do
    case "$1" in
        --expectations) expectations="${2:-}"; shift 2 ;;
        --rpc) rpcs+=("${2:-}"); shift 2 ;;
        *) die "usage: $0 [--expectations FILE] --rpc NAME=URL [--rpc NAME=URL ...]" ;;
    esac
done
((${#rpcs[@]} > 0)) || die "at least one --rpc NAME=URL is required"
[[ -f "$expectations" ]] || die "Safe expectation file not found: $expectations"

require_command cast
require_command jq

jq -e '.configured == true' "$expectations" >/dev/null || \
    die "Safe expectations are not configured; Finance must commit the verified treasury, owners, threshold, and proxy code hash"

treasury="$(jq -er '.treasury' "$expectations")"
threshold_expected="$(jq -er '.threshold' "$expectations")"
mapfile -t owners_expected < <(jq -r '.owners[] | ascii_downcase' "$expectations" | sort)
mapfile -t code_hashes_expected < <(jq -r '.proxy_code_hashes[] | ascii_downcase' "$expectations")
[[ "$treasury" != "0x0000000000000000000000000000000000000000" ]] || die "treasury is zero"
((${#owners_expected[@]} > 0)) || die "Safe owners are empty"
((threshold_expected > 0 && threshold_expected <= ${#owners_expected[@]})) || die "invalid Safe threshold"
((${#code_hashes_expected[@]} > 0)) || die "Safe proxy code hash allow-list is empty"

reports="$(mktemp "${TMPDIR:-/tmp}/crypto-topup-safe-report.XXXXXX")"
trap 'rm -f "$reports"' EXIT
status=0

for entry in "${rpcs[@]}"; do
    [[ "$entry" == *=* ]] || die "RPC must be NAME=URL: $entry"
    name="${entry%%=*}"
    rpc_url="${entry#*=}"
    actual_code_hash="$(lower "$(code_hash "$rpc_url" "$treasury")")"
    code_hash_ok=false
    for expected in "${code_hashes_expected[@]}"; do
        if [[ "$actual_code_hash" == "$expected" ]]; then
            code_hash_ok=true
            break
        fi
    done

    owners_json='[]'
    threshold_actual=""
    calls_ok=true
    if owners_call="$(cast call "$treasury" 'getOwners()(address[])' --json --rpc-url "$rpc_url" 2>/dev/null)" &&
        threshold_call="$(cast call "$treasury" 'getThreshold()(uint256)' --json --rpc-url "$rpc_url" 2>/dev/null)"; then
        owners_json="$(jq -c '.[0] | map(ascii_downcase) | sort' <<<"$owners_call")"
        threshold_actual="$(jq -r '.[0]' <<<"$threshold_call")"
    else
        calls_ok=false
    fi

    expected_owners_json="$(printf '%s\n' "${owners_expected[@]}" | jq -R . | jq -sc 'sort')"
    owners_ok=false
    threshold_ok=false
    [[ "$calls_ok" == true && "$owners_json" == "$expected_owners_json" ]] && owners_ok=true
    [[ "$calls_ok" == true && "$threshold_actual" == "$threshold_expected" ]] && threshold_ok=true
    passed=false
    if [[ "$code_hash_ok" == true && "$owners_ok" == true && "$threshold_ok" == true ]]; then
        passed=true
    else
        status=1
    fi

    jq -n \
        --arg chain "$name" \
        --arg treasury "$treasury" \
        --arg code_hash "$actual_code_hash" \
        --argjson owners "$owners_json" \
        --arg threshold "${threshold_actual:-unknown}" \
        --argjson code_hash_ok "$code_hash_ok" \
        --argjson owners_ok "$owners_ok" \
        --argjson threshold_ok "$threshold_ok" \
        --argjson passed "$passed" \
        '{chain: $chain, treasury: $treasury, code_hash: $code_hash,
          owners: $owners, threshold: $threshold,
          checks: {safe_proxy_code_hash: $code_hash_ok, owners: $owners_ok, threshold: $threshold_ok},
          passed: $passed}' >>"$reports"
done

jq -s --arg expectations "$expectations" \
    '{expectations: $expectations, chains: ., passed: all(.[]; .passed)}' "$reports"
exit "$status"
