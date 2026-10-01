#!/usr/bin/env bash
# deploy/phala-cvm.sh wait against a stub Phala Cloud CLI that answers `cvms get` with a sequence of
# CVM states and `logs --serial` with a console: an upgrade waits for the CVM to run a new compose;
# a provision (--unsealed) also accepts a settled CVM that does not run, but only one that booted
# (an instance id) and whose console's latest boot failed exactly as an unsealed topup CVM does.
# Any other boot failure fails at once; a CVM in progress, with the previous compose, or without
# the evidence times out after 60 polls.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
mkdir -p "$tmp/deploy" "$tmp/bin"
cp "$root/deploy/phala-cvm.sh" "$tmp/deploy/"
# The stub answers the Nth `cvms get` with line N of STUB_STATES, then repeats the last line.
cat >"$tmp/deploy/phala" <<'STUB'
#!/usr/bin/env bash
case "$*" in
    "cvms get cvm-1 --json")
        count=$(($(cat "$STUB_COUNT") + 1))
        echo "$count" >"$STUB_COUNT"
        mapfile -t states <"$STUB_STATES"
        ((count <= ${#states[@]})) || count=${#states[@]}
        printf '%s\n' "${states[count - 1]}"
        ;;
    "logs --serial --cvm-id cvm-1") cat "$STUB_SERIAL" ;;
    *) exit 1 ;;
esac
STUB
printf '#!/bin/sh\n' >"$tmp/bin/sleep"
chmod +x "$tmp/deploy/phala" "$tmp/bin/sleep"
export PATH="$tmp/bin:$PATH" STUB_COUNT="$tmp/count" STUB_STATES="$tmp/states" STUB_SERIAL="$tmp/serial"

# state STATUS IN_PROGRESS HASH [INSTANCE_ID]: one `cvms get` answer.
state() {
    printf '{"status": "%s", "in_progress": %s, "compose_hash": "%s", "instance_id": "%s"}\n' \
        "$1" "$2" "$3" "${4-i1}"
}
# console [LINES...]: the serial console of one boot up to `docker compose up`, then LINES.
console() {
    printf '%s\r\n' '[    0.000000] Linux version 6.9.0-dstack' 'Running pre-launch script' \
        '----------------------------------------------' \
        'Running Phala Cloud Pre-Launch Script v0.0.20' \
        '----------------------------------------------' 'Starting login process...' \
        'Pruning unused images' 'Starting containers' ' Network dstack_default  Created' \
        ' Container dstack-keys-1  Started' ' Container dstack-keys-1  Healthy' \
        ' Container dstack-postgres-1  Started' ' Container dstack-postgres-1  Waiting' "$@"
}
unsealed_failure=(' Container dstack-postgres-1  Error'
    'dependency failed to start: container dstack-postgres-1 is unhealthy'
    $'\e[0;1;31mFAILED\e[0m Failed to start \e[0;1;39mapp-compose.service\e[0m - App Compose Service.')
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

console "${unsealed_failure[@]}" >"$STUB_SERIAL"
unsealed=$({ state starting true "$new"; state error false "$new"; } | wait_for --unsealed cvm-1) ||
    { cat "$tmp/err" >&2; fail "a provision did not accept the CVM failed as unsealed"; }
[[ $(jq -r .status <<<"$unsealed") == error ]] || fail "a provision accepted the CVM while in progress"
redeployed=$({ state error false "$old"; state error true ab12; state stopped false ab12; } |
    wait_for --unsealed cvm-1 cd34) || fail "a provision's redeploy did not accept its new compose"
[[ $(jq -r .status <<<"$redeployed") == stopped ]] || fail "a provision's redeploy accepted the previous compose"
running=$(state running false "$new" | wait_for --unsealed cvm-1) || fail "a provision did not accept a running CVM"
[[ -n "$running" ]] || fail "a provision printed no CVM"

# The latest boot decides: an earlier boot's unsealed failure does not excuse this one's.
for unexpected in ' phala-pay Error pull access denied for phala-pay, repository does not exist' \
    'dependency failed to start: container dstack-keys-1 is unhealthy' \
    'Docker login failed: ghcr.io' \
    'Error response from daemon: manifest unknown'; do
    { console "${unsealed_failure[@]}"; console "$unexpected"; } >"$STUB_SERIAL"
    ! state error false "$new" | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted: $unexpected"
    grep -qF -- "$unexpected" "$tmp/err" || fail "a provision did not show the failure: $unexpected"
    [[ $(cat "$STUB_COUNT") == 1 ]] || fail "a provision waited on a failure: $unexpected"
done
console "${unsealed_failure[@]}" >"$STUB_SERIAL"
! state error false "$new" "" | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted a CVM that never booted"
console >"$STUB_SERIAL"
! state error false "$new" | wait_for --unsealed cvm-1 >/dev/null ||
    fail "a provision accepted a boot without app-compose's failure"
: >"$STUB_SERIAL"
! state error false "$new" | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted an empty console"
console "${unsealed_failure[@]}" >"$STUB_SERIAL"
! state error true "$new" | wait_for --unsealed cvm-1 >/dev/null || fail "a provision accepted a CVM in progress"
grep -q 'did not run or fail as unsealed within 15 minutes' "$tmp/err" ||
    fail "a provision's timeout does not say why"

running=$({ state error false "$new"; state running true "$new"; state running false "$new"; } |
    wait_for cvm-1) || fail "an upgrade did not accept the running CVM"
[[ $(jq -r '.in_progress' <<<"$running") == false ]] || fail "an upgrade accepted the CVM while in progress"
! state error false "$new" | wait_for cvm-1 >/dev/null || fail "an upgrade accepted a CVM that does not run"
grep -q 'did not run a new compose within 15 minutes' "$tmp/err" || fail "an upgrade's timeout does not say why"
! state running false "$old" | wait_for cvm-1 cd34 >/dev/null || fail "an upgrade accepted the previous compose"
[[ $(cat "$STUB_COUNT") == 60 ]] || fail "did not poll 60 times before timing out"
echo "CVM wait test passed"
