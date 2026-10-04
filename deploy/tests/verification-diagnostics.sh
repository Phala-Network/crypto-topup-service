#!/usr/bin/env bash
# Stub-only verification and the preflight forwarding path: no TOPUP, build, or network access.
set -euo pipefail
root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
source "$root/deploy/contracts/common.sh"
source "$root/deploy/preflight-rpc.sh"
tmp=$(mktemp -d "$root/.verification-tests.XXXXXX")
worker=""
cleanup() {
    if [[ -n "$worker" ]]; then
        # Release the bounded stub wait before joining the preflight worker.
        touch "$tmp/gate"
        wait "$worker" 2>/dev/null || true
    fi
    rm -rf "$tmp"
}
trap cleanup EXIT
export TMPDIR=$tmp CAST_STUB=$tmp/cast.py
export REFERENCE=$DEPLOY_CONTRACTS_DIR/reference.json TRACE=$tmp/trace
cat >"$CAST_STUB" <<'PY'
import json
import os
import sys
import time
from pathlib import Path

args = sys.argv[1:]
reference = json.loads(Path(os.environ['REFERENCE']).read_text())
with open(os.environ['TRACE'], 'a') as trace:
    trace.write(json.dumps(args) + '\n')
command = args[0]
if command == 'chain-id' and os.environ.get('GATE'):
    gate = Path(os.environ['GATE'])
    gate.with_suffix('.started').touch()
    deadline = time.monotonic() + 10
    while not gate.exists():
        if time.monotonic() >= deadline:
            sys.exit('stub gate was not released')
        time.sleep(0.02)
if command == os.environ.get('FAIL_COMMAND'):
    print('transport failure at ' + ' '.join(args), file=sys.stderr)
    sys.exit(1)
if command == 'chain-id':
    if os.environ.get('DELAY') and 'second' in args[-1]:
        time.sleep(1)
    print(11155111)
elif command == 'code':
    print(args[1])
elif command == 'keccak':
    field = next(field for field in ('proxy', 'factory', 'implementation')
                 if reference[field].lower() == args[1].lower())
    print(reference[field + '_code_hash'])
elif command == 'call':
    if args[2] == 'implementation()(address)':
        print(reference['implementation'])
    elif args[2] == 'factory()(address)':
        print(reference['factory'])
    else:
        print(next(vector['address'] for vector in reference['sample_forwarders']
                   if vector['treasury'] == args[3] and vector['salt'] == args[4]))
else:
    sys.exit('unexpected stub command')
PY
# common.sh prepends Foundry's bin directory; an exported function reliably intercepts cast.
cast() { python3 "$CAST_STUB" "$@"; }
export -f cast
verify=$DEPLOY_CONTRACTS_DIR/verify-deployment.sh
first='sepolia/a=https://rpc.invalid/secret-one'
second='sepolia/b=https://rpc.invalid/secret-second'
DELAY=1 "$verify" --rpc "$first" --rpc "$second" >"$tmp/pass.json" 2>"$tmp/pass.err"
jq -e --slurpfile ref "$REFERENCE" '
    .passed and .reference == $ref[0] and (.chains | length) == 2 and
    [.chains[].target] == ["sepolia/a", "sepolia/b"] and
    all(.chains[]; .passed and (.checks | all) and
        (.vectors | length) == ($ref[0].sample_forwarders | length) and all(.vectors[]; .passed))
' "$tmp/pass.json" >/dev/null
for stage in target chain-id proxy-code factory-code implementation-code implementation-binding factory-binding sample-forwarder-1 total; do
    grep -Eq "^verification: provider=2 stage=$stage status=passed elapsed_s=[0-9]+$" "$tmp/pass.err"
done
grep -Eq '^verification: provider=2 stage=chain-id status=passed elapsed_s=[1-9][0-9]*$' "$tmp/pass.err"
# Every original RPC/hash operation still executes, sequentially, on both providers.
expected=$(jq '.sample_forwarders | length' "$REFERENCE")
[[ $(wc -l <"$TRACE") -eq $((2 * (9 + expected))) ]]

# Transport failure still produces a JSON failure report and nonzero exit; raw errors stay private.
if FAIL_COMMAND=call "$verify" --rpc "$first" >"$tmp/fail.json" 2>"$tmp/fail.err"; then
    echo 'verification accepted failed binding/vector calls' >&2
    exit 1
fi
jq -e '.passed == false and .chains[0].checks.implementation == false and
    .chains[0].checks.sample_forwarders == false' "$tmp/fail.json" >/dev/null
grep -Eq 'stage=implementation-binding status=failed elapsed_s=[0-9]+' "$tmp/fail.err"
grep -Eq 'stage=total status=failed elapsed_s=[0-9]+' "$tmp/fail.err"
if FAIL_COMMAND=code "$verify" --rpc "$first" >"$tmp/code.json" 2>"$tmp/code.err"; then
    echo 'verification accepted failed code reads' >&2
    exit 1
fi

# Malformed names, missing separators/URLs, and unknown networks never echo the input.
for target in 'https://secret.invalid/key' 'sepolia/https://secret.invalid/key=https://rpc.invalid/key' \
    $'sepolia/a\nsecret=https://rpc.invalid/key' 'secret-unknown/a=https://rpc.invalid/key' 'sepolia/a='; do
    if "$verify" --rpc "$target" >"$tmp/invalid.json" 2>"$tmp/invalid.err"; then
        echo 'verification accepted a malformed target' >&2
        exit 1
    fi
    [[ ! -s "$tmp/invalid.json" ]]
    if grep -Eq 'secret|https://|rpc.invalid' "$tmp/invalid.err"; then
        echo 'malformed target leaked its input' >&2
        exit 1
    fi
done

# Exercise the exact helper used by preflight. Diagnostics must arrive before the verifier exits.
ok() { printf 'ok: %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; failures=$((failures + 1)); }
(
    export GATE=$tmp/gate
    failures=0
    check_contract_deployment "$tmp/live.json" 11155111 --rpc "$first"
    exit "$failures"
) >"$tmp/preflight.out" 2>"$tmp/preflight.err" &
worker=$!
python3 - "$tmp" <<'PY'
import sys
import time
from pathlib import Path

tmp = Path(sys.argv[1])
deadline = time.monotonic() + 8
while not (tmp / 'gate.started').exists():
    if time.monotonic() >= deadline:
        sys.exit('preflight did not reach the stub')
    time.sleep(0.02)
assert 'stage=chain-id status=started' in (tmp / 'preflight.err').read_text()
assert (tmp / 'live.json').stat().st_size == 0
(tmp / 'gate').touch()
PY
wait "$worker"
worker=""
jq -e '.passed' "$tmp/live.json" >/dev/null
(
    export FAIL_COMMAND=call
    failures=0
    check_contract_deployment "$tmp/preflight-fail.json" 11155111 --rpc "$first"
    exit "$failures"
) >"$tmp/preflight-fail.out" 2>"$tmp/preflight-fail.err" && {
    echo 'preflight lost verification failure' >&2
    exit 1
}
grep -Fq 'FAIL: verify-deployment.sh failed on chain 11155111' "$tmp/preflight-fail.err"
if grep -Eq 'secret|https://|transport failure' "$tmp"/*.err "$tmp"/*.out "$tmp"/*.json; then
    echo 'verification or preflight leaked provider credentials' >&2
    exit 1
fi
echo 'verification JSON, diagnostics, live preflight forwarding, failures, and redaction tests passed'
