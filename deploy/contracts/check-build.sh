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
    .solc == "0.8.37" and
    .evm_version == "cancun" and
    .optimizer == true and
    .optimizer_runs == 200 and
    .bytecode_hash == "none" and
    .cbor_metadata == false
' >/dev/null <<<"$config" || die "Foundry compiler settings drifted from the deterministic deployment profile"

# --ast adds the AST to the artifacts (bytecode is unchanged) so immutable references can be named.
(cd "$CONTRACTS_DIR" && forge build --ast >/dev/null)

# Prints {name: [offsets]} for every immutable of one contract's runtime code. Every reference must
# be a full 32-byte word: the service zeroes exactly these words before comparing code hashes.
immutable_offsets() {
    local contract="$1"
    jq -ce --arg contract "$contract" '
        [.ast.nodes[]
            | select(.nodeType == "ContractDefinition" and .name == $contract)
            | .nodes[]
            | select(.nodeType == "VariableDeclaration" and .mutability == "immutable")
            | {id: (.id | tostring), name}] as $immutables
        | .deployedBytecode.immutableReferences as $references
        | if ($references | keys | sort) != ($immutables | map(.id) | sort) then
              error("immutable references do not match declared immutables")
          elif ([$references[][] | .length] | all(. == 32)) | not then
              error("immutable reference is not a 32-byte word")
          else
              $immutables | map({key: .name, value: [$references[.id][] | .start]}) | from_entries
          end
    ' "$CONTRACTS_DIR/out/$contract.sol/$contract.json"
}
# Read before `forge inspect`, which may rewrite the artifacts without the AST.
factory_immutables="$(immutable_offsets ForwarderFactory)"
forwarder_immutables="$(immutable_offsets Forwarder)"

factory_creation="$(cd "$CONTRACTS_DIR" && forge inspect ForwarderFactory bytecode)"
factory_runtime="$(cd "$CONTRACTS_DIR" && forge inspect ForwarderFactory deployedBytecode)"
forwarder_creation="$(cd "$CONTRACTS_DIR" && forge inspect Forwarder bytecode)"
forwarder_runtime="$(cd "$CONTRACTS_DIR" && forge inspect Forwarder deployedBytecode)"
solc_version="$(jq -r '.metadata.compiler.version' "$CONTRACTS_DIR/out/ForwarderFactory.sol/ForwarderFactory.json")"

tmp="$(mktemp "${TMPDIR:-/tmp}/phala-pay-codehashes.XXXXXX")"
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
    --argjson factory_immutables "$factory_immutables" \
    --argjson forwarder_immutables "$forwarder_immutables" \
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
                runtime_template_code_hash: $factory_runtime_template_hash,
                immutable_offsets: $factory_immutables
            },
            Forwarder: {
                creation_code_hash: $forwarder_creation_hash,
                runtime_template_code_hash: $forwarder_runtime_template_hash,
                immutable_offsets: $forwarder_immutables
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
