#!/usr/bin/env bash
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
topup=${TOPUP_BIN:-$root/target/debug/topup}
tmp=$(mktemp -d)
trap 'find "$tmp" -depth -delete' EXIT INT TERM

if [[ ! -x "$topup" ]]; then
    cargo build --locked -q -p topup --manifest-path "$root/Cargo.toml"
fi

"$topup" --help >"$tmp/root.help"
"$topup" run --help >"$tmp/run.help"
"$topup" migrate --help >"$tmp/migrate.help"
"$topup" route --help >"$tmp/route.help"
"$topup" route validate --help >"$tmp/route-validate.help"
"$topup" outbox --help >"$tmp/outbox.help"
"$topup" outbox replay --help >"$tmp/outbox-replay.help"
"$topup" attest --help >"$tmp/attest.help"
"$topup" restore-check --help >"$tmp/restore-check.help"

fail=0
while IFS= read -r invocation; do
    command=${invocation#topup }
    case "$command" in
        run*) help="$tmp/run.help" ;;
        migrate*) help="$tmp/migrate.help" ;;
        "route validate"*) help="$tmp/route-validate.help" ;;
        route*) help="$tmp/route.help" ;;
        "outbox replay"*) help="$tmp/outbox-replay.help" ;;
        outbox*) help="$tmp/outbox.help" ;;
        attest*) help="$tmp/attest.help" ;;
        restore-check*) help="$tmp/restore-check.help" ;;
        *)
            echo "unknown topup invocation in runbooks: $invocation" >&2
            fail=1
            continue
            ;;
    esac

    subcommand=${command%% *}
    grep -Eq "(^|[[:space:]])${subcommand}([[:space:]]|$)" "$tmp/root.help" || {
        echo "missing topup subcommand: $subcommand" >&2
        fail=1
    }
    while IFS= read -r flag; do
        grep -F -- "$flag" "$help" >/dev/null || {
            echo "missing flag $flag for: $invocation" >&2
            fail=1
        }
    done < <(printf '%s\n' "$invocation" | grep -Eo -- '--[a-z0-9-]+' | sort -u)
done < <(
    grep -RhoE 'topup (run|migrate|route( validate)?|outbox( replay)?|attest|restore-check)( [^`[:space:]]+| --[a-z0-9-]+(=[^`[:space:]]+)?)*' \
        "$root/deploy/runbooks"/*.md "$root/deploy/runbooks/exercises"/*.md \
        | sed -E 's/[[:space:]]+$//' \
        | sort -u
)

[[ "$fail" -eq 0 ]] || exit 1
echo "runbook CLI references match topup --help"
