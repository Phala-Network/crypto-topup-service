#!/usr/bin/env bash
# Sets an account's treasury of one chain from an EOA key (design D10), for local and sandbox
# stacks whose treasury is an Anvil or test key: requests the EIP-4361 challenge, signs it with
# `cast wallet sign` (EIP-191 personal_sign), and submits it. A Safe treasury is set with the
# Safe{Core} SDK instead (docs/integration.md, "Treasuries").
# Usage: set-treasury.sh --api URL --key-file FILE --chain-id ID --private-key HEX
set -euo pipefail

usage() { echo "usage: $0 --api URL --key-file FILE --chain-id ID --private-key HEX" >&2; exit 2; }
api="" key_file="" chain_id="" private_key=""
while (($#)); do
    case "$1" in
        --api) api=${2:?}; shift 2 ;;
        --key-file) key_file=${2:?}; shift 2 ;;
        --chain-id) chain_id=${2:?}; shift 2 ;;
        --private-key) private_key=${2:?}; shift 2 ;;
        *) usage ;;
    esac
done
[[ -n "$api" && -r "$key_file" && "$chain_id" =~ ^[0-9]+$ && -n "$private_key" ]] || usage

auth="Authorization: Bearer $(<"$key_file")"
address=$(cast wallet address --private-key "$private_key")
challenge=$(jq -n --argjson chain_id "$chain_id" --arg address "$address" \
        '{chain_id: $chain_id, address: $address}' |
    curl --fail-with-body -sS -X POST -H "$auth" -H 'content-type: application/json' \
        --data-binary @- "$api/v1/treasuries/challenge")
message=$(jq -er .message <<<"$challenge")
signature=$(cast wallet sign --private-key "$private_key" "$message")
jq -n --argjson chain_id "$chain_id" --arg message "$message" --arg signature "$signature" \
        '{chain_id: $chain_id, message: $message, signature: $signature}' |
    curl --fail-with-body -sS -X POST -H "$auth" -H 'content-type: application/json' \
        --data-binary @- "$api/v1/treasuries"
echo
