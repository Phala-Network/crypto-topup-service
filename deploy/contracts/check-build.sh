#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

mode="${1:---check}"
case "$mode" in
    --check | --write) ;;
    *) die "usage: $0 [--check|--write]" ;;
esac

require_command forge
require_command cast
require_command jq

config="$(cd "$CONTRACTS_DIR" && forge config --json)"
jq -e '
    .solc == "0.8.24" and
    .evm_version == "cancun" and
    .optimizer == true and
    .optimizer_runs == 200 and
    .bytecode_hash == "none" and
    .cbor_metadata == false
' >/dev/null <<<"$config" || die "Foundry compiler settings drifted from the deterministic deployment profile"

(cd "$CONTRACTS_DIR" && forge build >/dev/null)

factory_creation="$(cd "$CONTRACTS_DIR" && forge inspect ForwarderFactory bytecode)"
factory_runtime="$(cd "$CONTRACTS_DIR" && forge inspect ForwarderFactory deployedBytecode)"
forwarder_creation="$(cd "$CONTRACTS_DIR" && forge inspect Forwarder bytecode)"
forwarder_runtime="$(cd "$CONTRACTS_DIR" && forge inspect Forwarder deployedBytecode)"
solc_version="$(jq -r '.metadata.compiler.version' "$CONTRACTS_DIR/out/ForwarderFactory.sol/ForwarderFactory.json")"

tmp="$(mktemp "${TMPDIR:-/tmp}/crypto-topup-codehashes.XXXXXX")"
trap 'rm -f "$tmp"' EXIT

jq -n \
    --arg solc "$solc_version" \
    --arg proxy "$DETERMINISTIC_PROXY" \
    --arg proxy_hash "$DETERMINISTIC_PROXY_CODE_HASH" \
    --arg salt "$FACTORY_SALT" \
    --arg factory_creation_hash "$(cast keccak "$factory_creation")" \
    --arg factory_runtime_template_hash "$(cast keccak "$factory_runtime")" \
    --arg forwarder_creation_hash "$(cast keccak "$forwarder_creation")" \
    --arg forwarder_runtime_template_hash "$(cast keccak "$forwarder_runtime")" \
    '{
        compiler: {
            solc: $solc,
            evm_version: "cancun",
            optimizer: true,
            optimizer_runs: 200,
            bytecode_hash: "none",
            cbor_metadata: false
        },
        deterministic_proxy: {
            address: $proxy,
            runtime_code_hash: $proxy_hash
        },
        factory_salt: $salt,
        artifacts: {
            ForwarderFactory: {
                creation_code_hash: $factory_creation_hash,
                runtime_template_code_hash: $factory_runtime_template_hash
            },
            Forwarder: {
                creation_code_hash: $forwarder_creation_hash,
                runtime_template_code_hash: $forwarder_runtime_template_hash
            }
        }
    }' >"$tmp"

expected="$DEPLOY_CONTRACTS_DIR/expected-codehashes.json"
if [[ "$mode" == "--write" ]]; then
    mv "$tmp" "$expected"
    trap - EXIT
    printf 'wrote %s\n' "$expected"
else
    [[ -f "$expected" ]] || die "missing $expected; run $0 --write"
    diff -u "$expected" "$tmp" || die "build fingerprints drifted; review and run $0 --write"
    printf 'deterministic build settings and code hashes match %s\n' "$expected"
fi
