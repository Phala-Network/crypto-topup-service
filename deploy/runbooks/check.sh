#!/usr/bin/env bash
set -euo pipefail

root=$(CDPATH= cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

export CARGO_TARGET_DIR="$tmp/target"
cargo build --locked -q -p topup --manifest-path "$root/Cargo.toml"
topup="$CARGO_TARGET_DIR/debug/topup"

"$topup" --help > "$tmp/root.help"
"$topup" run --help > "$tmp/run.help"
"$topup" migrate --help > "$tmp/migrate.help"
"$topup" route --help > "$tmp/route.help"
"$topup" route validate --help > "$tmp/route-validate.help"
"$topup" outbox --help > "$tmp/outbox.help"
"$topup" outbox replay --help > "$tmp/outbox-replay.help"
"$topup" attest --help > "$tmp/attest.help"
"$topup" restore-check --help > "$tmp/restore-check.help"

jq -r '
  .paths | to_entries[] | .key as $path
  | .value | keys[] | select(. != "parameters")
  | ascii_upcase + " " + $path
' "$root/crates/topup/openapi.json" | sort -u > "$tmp/openapi.operations"

extract_commands() {
    awk '
        /^```(sh|bash|shell)[[:space:]]*$/ { in_shell = 1; next }
        /^```/ {
            if (in_shell && continued != "") print continued
            in_shell = 0
            continued = ""
            next
        }
        in_shell {
            line = $0
            if (continued != "") line = continued " " line
            if (line ~ /\\[[:space:]]*$/) {
                sub(/\\[[:space:]]*$/, "", line)
                continued = line
                next
            }
            print line
            continued = ""
        }
    ' "$@"
}

clean_token() {
    local token=$1
    token=${token#\"}
    token=${token#\'}
    token=${token#\(}
    while [[ "$token" == *\" || "$token" == *\' || "$token" == *')' || "$token" == *';' || "$token" == *',' ]]; do
        token=${token%?}
    done
    printf '%s' "$token"
}

path_matches() {
    local actual=$1 spec=$2 index
    local -a actual_parts spec_parts
    IFS=/ read -r -a actual_parts <<< "$actual"
    IFS=/ read -r -a spec_parts <<< "$spec"
    [[ ${#actual_parts[@]} -eq ${#spec_parts[@]} ]] || return 1
    for index in "${!spec_parts[@]}"; do
        if [[ "${spec_parts[$index]}" == \{*\} ]]; then
            [[ -n "${actual_parts[$index]}" ]] || return 1
        elif [[ "${actual_parts[$index]}" != "${spec_parts[$index]}" ]]; then
            return 1
        fi
    done
}

api_operation_exists() {
    local method=$1 actual=$2 spec_method spec_path
    while read -r spec_method spec_path; do
        if [[ "$method" == "$spec_method" ]] && path_matches "$actual" "$spec_path"; then
            return 0
        fi
    done < "$tmp/openapi.operations"
    return 1
}

validate_topup() {
    local line=$1 source=$2 fail=0 index last=-1 token command nested help flag
    local -a words
    read -r -a words <<< "$line"
    for index in "${!words[@]}"; do
        token=$(clean_token "${words[$index]}")
        [[ "$token" == topup ]] || continue
        if (( index == 0 )); then
            last=$index
        elif (( index + 1 < ${#words[@]} )); then
            command=$(clean_token "${words[$((index + 1))]}")
            [[ "$command" != -* && "$command" != '<'* ]] && last=$index
        fi
    done
    (( last >= 0 )) || return 0
    (( last + 1 < ${#words[@]} )) || {
        echo "$source: incomplete topup invocation: $line" >&2
        return 1
    }

    command=$(clean_token "${words[$((last + 1))]}")
    if ! grep -Eq "^  ${command}([[:space:]]|$)" "$tmp/root.help"; then
        echo "$source: unknown topup subcommand: $command" >&2
        fail=1
        help=$tmp/root.help
    else
        case "$command" in
            run|migrate|attest|restore-check)
                help="$tmp/$command.help"
                ;;
            route|outbox)
                if (( last + 2 >= ${#words[@]} )); then
                    echo "$source: missing nested subcommand for topup $command" >&2
                    return 1
                fi
                nested=$(clean_token "${words[$((last + 2))]}")
                if ! grep -Eq "^  ${nested}([[:space:]]|$)" "$tmp/$command.help"; then
                    echo "$source: unknown topup nested subcommand: $command $nested" >&2
                    fail=1
                    help="$tmp/$command.help"
                else
                    help="$tmp/$command-$nested.help"
                fi
                ;;
        esac
    fi

    for ((index = last + 1; index < ${#words[@]}; index++)); do
        token=$(clean_token "${words[$index]}")
        [[ "$token" == --* ]] || continue
        flag=${token%%=*}
        if ! grep -Eq -- "(^|[[:space:]])${flag}([[:space:]<]|$)" "$help"; then
            echo "$source: missing flag $flag for: $line" >&2
            fail=1
        fi
    done
    return "$fail"
}

validate_curl() {
    local line=$1 source=$2 fail=0 index token method=GET path
    local -a words
    [[ "$line" =~ (^|[[:space:]])curl([[:space:]]|$) ]] || return 0
    read -r -a words <<< "$line"
    for index in "${!words[@]}"; do
        token=$(clean_token "${words[$index]}")
        case "$token" in
            -X|--request)
                if (( index + 1 < ${#words[@]} )); then
                    method=$(clean_token "${words[$((index + 1))]}")
                    method=${method^^}
                fi
                ;;
            --request=*)
                method=${token#--request=}
                method=${method^^}
                ;;
        esac
    done

    for token in "${words[@]}"; do
        token=$(clean_token "$token")
        [[ "$token" == *'/v1/'* ]] || continue
        path="/v1/${token#*/v1/}"
        path=${path%%\?*}
        if ! api_operation_exists "$method" "$path"; then
            echo "$source: unknown API operation: $method $path" >&2
            fail=1
        fi
    done
    return "$fail"
}

validate_commands() {
    local commands=$1 source=$2 fail=0 line
    while IFS= read -r line; do
        validate_topup "$line" "$source" || fail=1
        validate_curl "$line" "$source" || fail=1
    done < "$commands"
    return "$fail"
}

mapfile -t runbook_files < <(
    find "$root/deploy/runbooks" -maxdepth 1 -name '*.md' -print
    find "$root/deploy/runbooks/exercises" -maxdepth 1 -name '*.md' -print
)
extract_commands "${runbook_files[@]}" > "$tmp/runbook.commands"
validate_commands "$tmp/runbook.commands" runbooks

negative="$root/deploy/runbooks/tests/invalid.md"
extract_commands "$negative" > "$tmp/invalid.commands"
if validate_commands "$tmp/invalid.commands" negative-fixture > "$tmp/negative.out" 2>&1; then
    echo "negative fixture unexpectedly passed" >&2
    exit 1
fi
grep -F 'unknown topup subcommand: bogus' "$tmp/negative.out" >/dev/null
grep -F 'missing flag --bogus' "$tmp/negative.out" >/dev/null
grep -F 'unknown API operation: POST /v1/admin/not-a-route' "$tmp/negative.out" >/dev/null

echo "runbook CLI references match current-source topup --help"
echo "runbook API references match crates/topup/openapi.json"
echo "runbook negative command/flag/path fixture failed as expected"
