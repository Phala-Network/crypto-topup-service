#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

admin=""
treasury=""
output=""
while (($#)); do
    case "$1" in
        --admin) admin="${2:-}"; shift 2 ;;
        --treasury) treasury="${2:-}"; shift 2 ;;
        --output) output="${2:-}"; shift 2 ;;
        *) die "usage: $0 --admin ADDRESS --treasury ADDRESS [--output FILE]" ;;
    esac
done
[[ -n "$admin" && -n "$treasury" ]] || die "--admin and --treasury are required"

require_command anvil
require_command cast
require_command forge
require_command jq

tmp_dir="$(mktemp -d "${TMPDIR:-/tmp}/crypto-topup-reference.XXXXXX")"
anvil_pid=""
cleanup() {
    if [[ -n "$anvil_pid" ]]; then
        kill "$anvil_pid" 2>/dev/null || true
        wait "$anvil_pid" 2>/dev/null || true
    fi
    rm -rf "$tmp_dir"
}
trap cleanup EXIT INT TERM

port="$(find_free_port)"
rpc_url="http://127.0.0.1:$port"
anvil --silent --disable-default-create2-deployer --port "$port" --chain-id 31337 \
    >"$tmp_dir/anvil.log" 2>&1 &
anvil_pid=$!
wait_for_rpc "$rpc_url"

"$DEPLOY_CONTRACTS_DIR/deploy-proxy.sh" --rpc-url "$rpc_url" --local-fund --broadcast >&2

factory="$(predicted_factory "$admin" "$treasury")"
implementation="$(predicted_implementation "$factory")"
init_code_hash="$(cast keccak "$(factory_init_code "$admin" "$treasury")")"

(
    cd "$CONTRACTS_DIR"
    ADMIN="$admin" TREASURY="$treasury" forge script \
        script/DeployFactory.s.sol:DeployFactory \
        --rpc-url "$rpc_url" \
        --broadcast \
        --private-key "$ANVIL_PRIVATE_KEY" \
        -q
) >&2

actual_implementation="$(cast call "$factory" 'implementation()(address)' --rpc-url "$rpc_url")"
[[ "$(lower "$actual_implementation")" == "$(lower "$implementation")" ]] || \
    die "reference deployment implementation mismatch"

vectors='[]'
while IFS= read -r salt; do
    address="$(cast call "$factory" 'addressOf(bytes32)(address)' "$salt" --rpc-url "$rpc_url")"
    vectors="$(jq -c --arg salt "$salt" --arg address "$address" \
        '. + [{salt: $salt, address: $address}]' <<<"$vectors")"
done < <(jq -r '.salts[]' "$CONTRACTS_DIR/test-vectors/create2.json")

manifest="$(jq -n \
    --arg admin "$admin" \
    --arg treasury "$treasury" \
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
        admin: $admin,
        treasury: $treasury,
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

if [[ -n "$output" ]]; then
    printf '%s\n' "$manifest" | jq . >"$output"
else
    printf '%s\n' "$manifest" | jq .
fi
