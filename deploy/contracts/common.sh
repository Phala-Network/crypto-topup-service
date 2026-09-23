#!/usr/bin/env bash

set -euo pipefail

DEPLOY_CONTRACTS_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$DEPLOY_CONTRACTS_DIR/../.." && pwd)"
CONTRACTS_DIR="$REPO_ROOT/contracts"

DETERMINISTIC_PROXY="0x4e59b44847b379578588920cA78FbF26c0B4956C"
DETERMINISTIC_PROXY_CODE_HASH="0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989"
FACTORY_SALT="0x33f357abc669d0dae6ca878fa2e4435dd82ff4a983efd8a8dd4f2efa9437a426"
ANVIL_PRIVATE_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"
ZERO_ADDRESS="0x0000000000000000000000000000000000000000"
ZERO_HASH="0x0000000000000000000000000000000000000000000000000000000000000000"

export PATH="$HOME/.foundry/bin:$HOME/.cargo/bin:$PATH"

die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

require_command() {
    command -v "$1" >/dev/null 2>&1 || die "required command not found: $1"
}

lower() {
    tr '[:upper:]' '[:lower:]' <<<"$1"
}

factory_init_code() {
    local admin="$1"
    local treasury="$2"
    local bytecode constructor_args

    bytecode="$(cd "$CONTRACTS_DIR" && forge inspect ForwarderFactory bytecode)"
    constructor_args="$(cast abi-encode 'constructor(address,address)' "$admin" "$treasury")"
    printf '%s%s\n' "$bytecode" "${constructor_args#0x}"
}

predicted_factory() {
    local admin="$1"
    local treasury="$2"
    local init_code

    init_code="$(factory_init_code "$admin" "$treasury")"
    cast compute-address "$DETERMINISTIC_PROXY" --salt "$FACTORY_SALT" --init-code "$init_code"
}

predicted_implementation() {
    cast compute-address "$1" --nonce 1
}

code_hash() {
    local rpc_url="$1"
    local address="$2"
    local code

    code="$(cast code "$address" --rpc-url "$rpc_url")"
    if [[ "$code" == "0x" ]]; then
        printf '%s\n' "$ZERO_HASH"
    else
        cast keccak "$code"
    fi
}

# Starts a local anvil in the background on a port the kernel picks (--port 0) and reads the
# bound address from its log, so concurrent jobs on a shared runner cannot race for a port.
# Call it directly, not in $(...): it sets ANVIL_PID and ANVIL_RPC_URL in the caller's shell.
# ANVIL_PID is set as soon as anvil is spawned so a caller's signal trap can stop it.
# Usage: start_anvil LOG_FILE [ANVIL_ARGS...]
start_anvil() {
    local log="$1"
    local address
    shift

    anvil --host 127.0.0.1 --port 0 "$@" >"$log" 2>&1 &
    ANVIL_PID=$!
    ANVIL_RPC_URL=""
    for _ in $(seq 1 100); do
        if [[ -z "$ANVIL_RPC_URL" ]]; then
            address="$(sed -n 's/^Listening on \(127\.0\.0\.1:[0-9][0-9]*\)$/\1/p' "$log")"
            [[ -z "$address" ]] || ANVIL_RPC_URL="http://$address"
        fi
        if [[ -n "$ANVIL_RPC_URL" ]] && cast chain-id --rpc-url "$ANVIL_RPC_URL" >/dev/null 2>&1; then
            return 0
        fi
        kill -0 "$ANVIL_PID" 2>/dev/null || break
        sleep 0.1
    done
    kill "$ANVIL_PID" 2>/dev/null || true
    wait "$ANVIL_PID" 2>/dev/null || true
    die "anvil did not become ready; last log lines: $(tail -n 5 "$log")"
}

is_address() {
    [[ "$1" =~ ^0x[0-9a-fA-F]{40}$ ]]
}

# Statically validates a configured expectations file and exports the approved factory
# constructor inputs as EXPECTED_ADMIN and EXPECTED_TREASURY.
load_expectations() {
    local file="$1"
    local problems

    require_command jq
    [[ -f "$file" ]] || die "expectations file not found: $file"
    jq -e '.configured == true' "$file" >/dev/null || \
        die "expectations are not configured; Finance must commit the approved networks, admin and treasury Safes, owners, threshold, proxy code hashes, and singleton"

    problems="$(jq -r --arg zero_address "$ZERO_ADDRESS" --arg zero_hash "$ZERO_HASH" '
        def address: type == "string" and test("^0x[0-9a-fA-F]{40}$") and ascii_downcase != $zero_address;
        def hash: type == "string" and test("^0x[0-9a-fA-F]{64}$") and ascii_downcase != $zero_hash;
        def address_or_zero: type == "string" and test("^0x[0-9a-fA-F]{40}$");
        def safe_entries($target): [.safes[] | select(.address | ascii_downcase == ($target | ascii_downcase))];
        if (.networks | type) != "object" or (.networks | length) == 0 or
            any(.networks[]; (.chain_id | type) != "number" or .chain_id <= 0 or .chain_id != (.chain_id | floor))
        then "networks must map each target network to a positive integer chain_id" else empty end,
        if (.admin | address) then empty else "admin must be a non-zero address" end,
        if (.treasury | address) then empty else "treasury must be a non-zero address" end,
        if (.safes | type) != "array" or (.safes | length) == 0 then "safes must list the approved Safes"
        else
            (.safes[] |
                if (.address | address) and (.owners | type == "array" and length > 0 and all(address)) and
                    (.owners | map(ascii_downcase) | unique | length) == (.owners | length) and
                    (.threshold | type == "number") and .threshold > 0 and .threshold <= (.owners | length) and
                    (.proxy_code_hashes | type == "array" and length > 0 and all(hash)) and
                    (.singleton | address) and (.singleton_code_hash | hash) and
                    (.modules | type == "array" and all(address)) and
                    (.modules | map(ascii_downcase) | unique | length) == (.modules | length) and
                    (.guard | address_or_zero) and (.fallback_handler | address_or_zero)
                then empty
                else "invalid Safe entry \(.address | tostring): need address, unique owners, 0 < threshold <= owners, proxy_code_hashes, singleton, singleton_code_hash, unique modules (may be empty), guard, and fallback_handler (zero address for none)"
                end),
            if (.admin | address) and (safe_entries(.admin) | length) != 1
            then "admin must match exactly one approved Safe entry" else empty end,
            if (.treasury | address) and (safe_entries(.treasury) | length) != 1
            then "treasury must match exactly one approved Safe entry" else empty end
        end
    ' "$file" 2>/dev/null)" || die "malformed expectations file: $file"
    [[ -z "$problems" ]] || die "invalid expectations in $file: ${problems//$'\n'/; }"

    EXPECTED_ADMIN="$(jq -r '.admin' "$file")"
    EXPECTED_TREASURY="$(jq -r '.treasury' "$file")"
}

# Parses NETWORK[/LABEL]=URL and resolves the network's committed chain id into TARGET_NAME,
# TARGET_NETWORK, TARGET_RPC_URL, and TARGET_EXPECTED_CHAIN_ID.
parse_target() {
    local file="$1"
    local entry="$2"

    [[ "$entry" == *=* ]] || die "RPC must be NETWORK[/LABEL]=URL: $entry"
    TARGET_NAME="${entry%%=*}"
    TARGET_RPC_URL="${entry#*=}"
    TARGET_NETWORK="${TARGET_NAME%%/*}"
    [[ -n "$TARGET_NETWORK" && -n "$TARGET_RPC_URL" ]] || die "RPC must be NETWORK[/LABEL]=URL: $entry"
    TARGET_EXPECTED_CHAIN_ID="$(jq -er --arg network "$TARGET_NETWORK" \
        '.networks[$network].chain_id' "$file")" || \
        die "unknown network '$TARGET_NETWORK'; expected one of: $(jq -r '.networks | keys | join(", ")' "$file")"
}

# Prints the chain id reported by the RPC, or "error" when the RPC cannot answer.
rpc_chain_id() {
    cast chain-id --rpc-url "$1" 2>/dev/null || printf 'error\n'
}

# Shared by deploy-factory.sh and verify-deployment.sh: binds ADMIN and TREASURY from the
# environment to the approved Safes in the expectations file, then verifies both Safes and the
# chain id on every target. Writes the verify-safe.sh report to REPORT and fails on any mismatch.
validate_deployment_params() {
    local expectations="$1"
    local report="$2"
    shift 2
    local safe_args=(--expectations "$expectations")
    local entry

    [[ -n "${ADMIN:-}" && -n "${TREASURY:-}" ]] || die "ADMIN and TREASURY must be set in the environment"
    is_address "$ADMIN" || die "ADMIN is not an address: $ADMIN"
    is_address "$TREASURY" || die "TREASURY is not an address: $TREASURY"
    load_expectations "$expectations"
    [[ "$(lower "$ADMIN")" == "$(lower "$EXPECTED_ADMIN")" ]] || \
        die "ADMIN $ADMIN does not match the approved admin Safe $EXPECTED_ADMIN in $expectations"
    [[ "$(lower "$TREASURY")" == "$(lower "$EXPECTED_TREASURY")" ]] || \
        die "TREASURY $TREASURY does not match the approved treasury Safe $EXPECTED_TREASURY in $expectations"

    for entry in "$@"; do
        safe_args+=(--rpc "$entry")
    done
    "$DEPLOY_CONTRACTS_DIR/verify-safe.sh" "${safe_args[@]}" >"$report" || {
        printf 'error: admin/treasury Safe verification failed; report:\n' >&2
        cat "$report" >&2
        return 1
    }
}
