#!/usr/bin/env bash

set -euo pipefail
source "$(dirname -- "$0")/common.sh"

ONE_TIME_SIGNER="0x3fab184622dc19b6109349b94811493bf2a45362"
RAW_DEPLOYMENT_TRANSACTION="0xf8a58085174876e800830186a08080b853604580600e600039806000f350fe7fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffe03601600081602082378035828234f58015156039578182fd5b8082525050506014600cf31ba02222222222222222222222222222222222222222222222222222222222222222a02222222222222222222222222222222222222222222222222222222222222222"
REQUIRED_BALANCE_WEI="10000000000000000"

rpc_url=""
broadcast=false
local_fund=false
while (($#)); do
    case "$1" in
        --rpc-url) rpc_url="${2:-}"; shift 2 ;;
        --broadcast) broadcast=true; shift ;;
        --local-fund) local_fund=true; shift ;;
        *) die "usage: $0 --rpc-url URL [--broadcast] [--local-fund]" ;;
    esac
done
[[ -n "$rpc_url" ]] || die "--rpc-url is required"

require_command cast

existing_hash="$(code_hash "$rpc_url" "$DETERMINISTIC_PROXY")"
if [[ "$existing_hash" != "0x0000000000000000000000000000000000000000000000000000000000000000" ]]; then
    [[ "$(lower "$existing_hash")" == "$(lower "$DETERMINISTIC_PROXY_CODE_HASH")" ]] || \
        die "deterministic proxy exists with unexpected code hash: $existing_hash"
    printf 'deterministic proxy already present at %s\n' "$DETERMINISTIC_PROXY"
    exit 0
fi

if [[ "$local_fund" == true ]]; then
    client="$(cast rpc --rpc-url "$rpc_url" web3_clientVersion | tr -d '"')"
    [[ "$(lower "$client")" == *anvil* ]] || die "--local-fund is restricted to Anvil"
    cast rpc --rpc-url "$rpc_url" anvil_setBalance "$ONE_TIME_SIGNER" \
        0x2386f26fc10000 >/dev/null
fi

balance="$(cast balance "$ONE_TIME_SIGNER" --rpc-url "$rpc_url")"
if ((balance < REQUIRED_BALANCE_WEI)); then
    cat >&2 <<EOF
The one-time deployment signer needs exactly the transaction gas budget before broadcast.
HUMAN-ONLY funding step:
  cast send $ONE_TIME_SIGNER --value 0.01ether --rpc-url \"\$RPC_URL\" --private-key \"\$PRIVATE_KEY\"
EOF
    die "one-time signer balance is below $REQUIRED_BALANCE_WEI wei"
fi

if [[ "$broadcast" != true ]]; then
    printf 'proxy absent; checks passed. Re-run with --broadcast to publish the upstream signed transaction.\n'
    exit 0
fi

printf 'broadcasting the upstream one-time signed proxy transaction\n' >&2
cast publish "$RAW_DEPLOYMENT_TRANSACTION" --rpc-url "$rpc_url" >/dev/null

for _ in $(seq 1 60); do
    if [[ "$(lower "$(code_hash "$rpc_url" "$DETERMINISTIC_PROXY")")" == \
        "$(lower "$DETERMINISTIC_PROXY_CODE_HASH")" ]]; then
        printf 'deployed deterministic proxy at %s\n' "$DETERMINISTIC_PROXY"
        exit 0
    fi
    sleep 0.2
done
die "proxy transaction was published but expected code did not appear"
