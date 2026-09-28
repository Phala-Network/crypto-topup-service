#!/usr/bin/env bash
# Checks the routes of a rendered topup compose against the Deploy environment it goes to
# (docs/design/multi-tenant.md D9): every route's `livemode` matches its chain, `true` on a known
# mainnet and `false` on a known public testnet, so production hosts test mode on Sepolia beside
# live mode on mainnet. Refused: a chain on neither list (add it here after review), a local
# development chain, a compose without routes, and any live route in staging, which moves no real
# money. The service validates `livemode` against the chain again when it loads a route.
#
# Usage: deploy/check-route-modes.sh staging|production COMPOSE
set -euo pipefail

environment=${1:?usage: $0 staging|production COMPOSE}
compose=${2:?usage: $0 staging|production COMPOSE}
case "$environment" in
    staging | production) ;;
    *) echo "unknown Deploy environment: $environment" >&2; exit 64 ;;
esac

# Ethereum, OP, Base, Arbitrum One.
mainnets=" 1 10 8453 42161 "
# Sepolia, Holesky, Hoodi, Base Sepolia, OP Sepolia.
testnets=" 11155111 17000 560048 84532 11155420 "
# Anvil and Hardhat, Geth dev: never deployed.
devnets=" 31337 1337 "

# One "route livemode chain_id" line per route file embedded in the compose. A route's keys are
# its `route:`, `livemode:`, and its chain's `chain_id:`; nothing else in the compose uses them.
routes=$(awk '
    function flush() {
        if (name != "") print name, (livemode == "" ? "-" : livemode), (chain == "" ? "-" : chain)
    }
    /^[[:space:]]+route:[[:space:]]/ { flush(); name = $2; livemode = ""; chain = ""; next }
    name != "" && /^[[:space:]]+livemode:[[:space:]]/ { livemode = $2 }
    name != "" && /^[[:space:]]+chain_id:[[:space:]]/ { chain = $2 }
    END { flush() }
' "$compose")
[[ -n "$routes" ]] || { echo "the compose carries no route" >&2; exit 1; }

failed=0
refuse() {
    echo "route $1: $2" >&2
    failed=1
}
while read -r route livemode chain; do
    case "$livemode" in
        true | false) ;;
        *) refuse "$route" "livemode must be true or false, not $livemode"; continue ;;
    esac
    if [[ "$mainnets" == *" $chain "* ]]; then
        [[ "$livemode" == true ]] || refuse "$route" "chain $chain is a mainnet: livemode must be true"
    elif [[ "$testnets" == *" $chain "* ]]; then
        [[ "$livemode" == false ]] || refuse "$route" "chain $chain is a test network: livemode must be false"
    elif [[ "$devnets" == *" $chain "* ]]; then
        refuse "$route" "chain $chain is a local development chain"
        continue
    else
        refuse "$route" "chain $chain is not a known mainnet or test network"
        continue
    fi
    if [[ "$environment" == staging && "$livemode" == true ]]; then
        refuse "$route" "staging serves test mode only; a live route belongs in production"
    fi
done <<<"$routes"
if ((failed)); then
    echo "the $environment compose's routes do not match their chains' modes" >&2
    exit 1
fi
while read -r route livemode chain; do
    echo "$route: chain $chain, livemode $livemode"
done <<<"$routes"
