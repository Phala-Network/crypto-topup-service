#!/usr/bin/env bash
# Runs the product settlement conformance suite (docs/conformance.md) against the staging
# reference product, deploy/product/reference_product, so it is held to the same contract as the
# Rust reference: `prepare` deploys the fixtures on a disposable Anvil, the product serves from a
# config built from the manifest with `conformance` on, and `run` exercises it, restarting the
# product through a supervisor loop to check retention. Everything it starts is stopped on exit.
# Usage: deploy/product/conformance.sh
#
# Requires Foundry (anvil, forge, cast) with contracts/lib checked out, cargo, curl, jq, and uv.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)
source "$root/deploy/contracts/common.sh"
for command in anvil forge cast cargo curl jq uv; do
    require_command "$command"
done

tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-product-conformance.XXXXXX")
supervisor=""
cleanup() {
    status=$?
    set +e
    if [[ -n "$supervisor" ]]; then
        touch "$tmp/stop"
        kill "$(<"$tmp/product.pid")" 2>/dev/null
        wait "$supervisor" 2>/dev/null
        ((status == 0)) || { echo "--- product log (last 40 lines) ---" >&2; tail -n 40 "$tmp/product.log" >&2; }
    fi
    [[ -n "${ANVIL_PID:-}" ]] && kill "$ANVIL_PID" 2>/dev/null && wait "$ANVIL_PID" 2>/dev/null
    rm -rf "$tmp"
    exit "$status"
}
trap cleanup EXIT INT TERM

conformance=(cargo run --locked --quiet -p topup-conformance --bin topup-conformance --)
start_anvil "$tmp/anvil.log" --chain-id 31337
"${conformance[@]}" prepare --rpc-url "$ANVIL_RPC_URL" --chain-id 31337 \
    --manifest "$tmp/manifest.json" >/dev/null

# The product's interpreter itself, so the restart hook's SIGTERM reaches the product directly.
python=$(uv run --locked --project "$root/sdk/python" --quiet python -c 'import sys; print(sys.executable)')
# The suite signs with the public `dev` seed; the product pins its public key.
settlement_key=$("$python" -c 'from topup_sdk import RequestSigner
print(RequestSigner.from_seed("settlement/v1", bytes([7] * 32)).public_key_base64())')
port=$(free_port)
jq --arg url "http://127.0.0.1:$port" --argjson port "$port" --arg key "$settlement_key" \
    --arg ledger "$tmp/ledger.sqlite3" \
    '{service_url: "http://service.invalid", product_slug, product_keyid: "conformance/v1",
      route, chain_id, rpc_url, factory, implementation, token: .asset_contract,
      token_symbol: "TEST", public_url: $url, listen_port: $port, ledger_path: $ledger,
      settlement_public_key: $key, driver_public_key: $key, per_deposit_cap_minor: 10000,
      per_period_cap_minor: 50000, period_seconds: 86400, conformance: true}' \
    "$tmp/manifest.json" >"$tmp/product.json"

# Starts the product again whenever it exits, so a restart is a SIGTERM to its process.
(
    while [[ ! -e "$tmp/stop" ]]; do
        PYTHONPATH="$root/deploy/product" "$python" -m reference_product serve \
            --config "$tmp/product.json" >>"$tmp/product.log" 2>&1 &
        echo "$!" >"$tmp/product.pid"
        wait "$!" || true
    done
) &
supervisor=$!
for _ in $(seq 1 100); do
    curl -fsS "http://127.0.0.1:$port/healthz" >/dev/null 2>&1 && break
    sleep 0.2
done

"${conformance[@]}" run --manifest "$tmp/manifest.json" \
    --settlement-url "http://127.0.0.1:$port/settlements" --signing-key dev \
    --keyid settlement/v1 --per-deposit-cap 10000 --per-period-cap 50000 \
    --period-seconds 86400 --report "$tmp/report.json" \
    --restart-command "kill \$(cat '$tmp/product.pid')"
