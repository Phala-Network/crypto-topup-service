#!/usr/bin/env bash
# Checks deploy/check-route-modes.sh: production hosts test routes on test networks beside live
# routes on mainnets, staging only test routes, and a route whose mode contradicts its chain, or
# on an unknown or local chain, is refused.
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
check="$root/deploy/check-route-modes.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

# compose FILE ROUTE:LIVEMODE:CHAIN...: a rendered compose whose inline topup.yaml lists one route
# per argument. Its `environment` always claims production: the check must use Deploy's.
compose() {
    local file=$1 spec route livemode chain
    shift
    {
        echo "services:"
        echo "  topup:"
        echo "    command: [topup, run, --config, /etc/topup/topup.yaml]"
        echo "configs:"
        echo "  topup_0123456789ab:"
        echo "    content: |"
        echo "      environment: production"
        echo "      routes:"
        for spec in "$@"; do
            IFS=: read -r route livemode chain <<<"$spec"
            echo "        - route: $route"
            echo "          version: 1"
            [[ "$livemode" == omit ]] || echo "          livemode: $livemode"
            echo "          chain:"
            echo "            chain_id: $chain"
        done
    } >"$file"
}

# accepts ENVIRONMENT SPEC...: the check passes.
accepts() {
    local environment=$1
    shift
    compose "$tmp/compose.yml" "$@"
    "$check" "$environment" "$tmp/compose.yml" >"$tmp/out" 2>&1 || {
        echo "check-route-modes refused $environment $*: $(cat "$tmp/out")" >&2
        exit 1
    }
}

# refuses ENVIRONMENT EXPECTED SPEC...: the check fails with EXPECTED in its output.
refuses() {
    local environment=$1 expected=$2
    shift 2
    compose "$tmp/compose.yml" "$@"
    if "$check" "$environment" "$tmp/compose.yml" >"$tmp/out" 2>&1; then
        echo "check-route-modes accepted $environment $*" >&2
        exit 1
    fi
    grep -F -- "$expected" "$tmp/out" >/dev/null || {
        echo "check-route-modes refused $environment $* for an unexpected reason: $(cat "$tmp/out")" >&2
        exit 1
    }
}

# One production deployment serves both modes.
accepts production sepolia-pha:false:11155111 mainnet-pha:true:1
accepts production mainnet-pha:true:1
accepts production base-usdc:true:8453 sepolia-pha:false:11155111
accepts staging sepolia-pha:false:11155111 hoodi-pha:false:560048
# The committed compose is a staging compose.
"$check" staging "$root/deploy/docker-compose.yml" >/dev/null

refuses production "chain 11155111 is a test network: livemode must be false" \
    mainnet-pha:true:1 sepolia-pha:true:11155111
refuses production "chain 1 is a mainnet: livemode must be true" mainnet-pha:false:1
refuses staging "chain 1 is a mainnet: livemode must be true" mainnet-pha:false:1
refuses staging "staging serves test mode only" sepolia-pha:false:11155111 mainnet-pha:true:1
refuses production "chain 31337 is a local development chain" anvil-pha:false:31337
refuses production "chain 56 is not a known mainnet or test network" bsc-usdt:true:56
refuses production "livemode must be true or false" mainnet-pha:omit:1
refuses production "the compose carries no route"
refuses preview "unknown Deploy environment" sepolia-pha:false:11155111

echo "route mode check test passed"
