#!/usr/bin/env bash
# deploy/phala-cvm.sh wait against a stub Phala Cloud CLI that answers `cvms get` with a sequence of
# CVM states: an upgrade waits for the CVM to run a new compose; a provision (--unsealed) for it to
# boot a new compose (an instance id), in any status. Neither accepts a CVM still in progress or with
# the previous compose, and both time out after 60 polls.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/deploy" "$tmp/bin"
cp "$root/deploy/phala-cvm.sh" "$tmp/deploy/"
# The stub answers the Nth `cvms get` with line N of STUB_STATES, then repeats the last line.
cat >"$tmp/deploy/phala" <<'STUB'
#!/usr/bin/env bash
[[ "$*" == "cvms get cvm-1 --json" ]] || exit 1
count=$(($(cat "$STUB_COUNT") + 1))
echo "$count" >"$STUB_COUNT"
mapfile -t states <"$STUB_STATES"
((count <= ${#states[@]})) || count=${#states[@]}
printf '%s\n' "${states[count - 1]}"
STUB
printf '#!/bin/sh\n' >"$tmp/bin/sleep"
chmod +x "$tmp/deploy/phala" "$tmp/bin/sleep"
export PATH="$tmp/bin:$PATH" STUB_COUNT="$tmp/count" STUB_STATES="$tmp/states"

# state STATUS IN_PROGRESS HASH [INSTANCE_ID]: one `cvms get` answer; the instance id may be null.
state() {
    printf '{"status": "%s", "in_progress": %s, "compose_hash": "%s", "instance_id": %s}\n' \
        "$1" "$2" "$3" "${4-\"i1\"}"
}
# wait_for ARGS... with the states on stdin: the CVM JSON it accepts, on stdout.
wait_for() {
    cat >"$STUB_STATES"
    echo 0 >"$STUB_COUNT"
    "$tmp/deploy/phala-cvm.sh" wait "$@" 2>"$tmp/err"
}
fail() {
    echo "phala-cvm wait: $*" >&2
    exit 1
}
new=0xAB12 old=0xcd34

booted=$({ state starting true "$new" null; state error false "$new"; } | wait_for --unsealed cvm-1) ||
    fail "a provision did not accept the settled CVM that booted the new compose"
[[ $(jq -r .status <<<"$booted") == error ]] || fail "a provision accepted the CVM while in progress"
redeployed=$({ state error false "$old"; state error true ab12; state stopped false ab12; } |
    wait_for --unsealed cvm-1 cd34) || fail "a provision's redeploy did not accept its new compose"
[[ $(jq -r .status <<<"$redeployed") == stopped ]] || fail "a provision's redeploy accepted the previous compose"
! state error false "$new" null | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted a CVM that never booted"
grep -q 'did not boot a new compose within 15 minutes' "$tmp/err" || fail "a provision's timeout does not say why"
! state starting true "$new" | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted a CVM in progress"
[[ $(cat "$STUB_COUNT") == 60 ]] || fail "a provision did not poll 60 times before timing out"

running=$({ state error false "$new"; state running true "$new"; state running false "$new"; } |
    wait_for cvm-1) || fail "an upgrade did not accept the running CVM"
[[ $(jq -r '.in_progress' <<<"$running") == false ]] || fail "an upgrade accepted the CVM while in progress"
! state error false "$new" | wait_for cvm-1 >/dev/null || fail "an upgrade accepted a CVM that does not run"
grep -q 'did not run a new compose within 15 minutes' "$tmp/err" || fail "an upgrade's timeout does not say why"
! state running false "$old" | wait_for cvm-1 cd34 >/dev/null || fail "an upgrade accepted the previous compose"
echo "CVM wait test passed"
