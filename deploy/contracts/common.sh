#!/usr/bin/env bash

set -euo pipefail

DEPLOY_CONTRACTS_DIR="$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "$DEPLOY_CONTRACTS_DIR/../.." && pwd)"
CONTRACTS_DIR="$REPO_ROOT/contracts"

DETERMINISTIC_PROXY="0x4e59b44847b379578588920cA78FbF26c0B4956C"
DETERMINISTIC_PROXY_CODE_HASH="0x2fa86add0aed31f33a762c9d88e807c475bd51d0f52bd0955754b2608f7e4989"
FACTORY_SALT="0x33f357abc669d0dae6ca878fa2e4435dd82ff4a983efd8a8dd4f2efa9437a426"
ANVIL_PRIVATE_KEY="0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80"

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
        printf '%s\n' "0x0000000000000000000000000000000000000000000000000000000000000000"
    else
        cast keccak "$code"
    fi
}

find_free_port() {
    local port
    for port in $(seq 19545 19645); do
        if ! ss -ltn "sport = :$port" 2>/dev/null | grep -q LISTEN; then
            printf '%s\n' "$port"
            return 0
        fi
    done
    die "could not find a free local port"
}

wait_for_rpc() {
    local rpc_url="$1"
    local attempt
    for attempt in $(seq 1 60); do
        if cast chain-id --rpc-url "$rpc_url" >/dev/null 2>&1; then
            return 0
        fi
        sleep 0.1
    done
    die "RPC did not become ready: $rpc_url"
}
