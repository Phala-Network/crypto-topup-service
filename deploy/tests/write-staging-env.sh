#!/usr/bin/env bash
# write-staging-env.sh writes exactly the staging.env.example names, refuses a missing required
# name or a value the CLI's dotenv parser would alter without printing any value, and accepts empty
# optional names.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
writer="$root/deploy/write-staging-env.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

names_of() {
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$1" | sort
}
names_of "$root/deploy/staging.env.example" >"$tmp/expected"

# Every name set to a distinctive value; the optional ones empty.
declare -a assignments=()
while IFS= read -r name; do
    case "$name" in
        AWS_SESSION_TOKEN | AWS_ENDPOINT | COINMETRICS_API_KEY) assignments+=("$name=") ;;
        *) assignments+=("$name=secret-value-of-$name") ;;
    esac
done <"$tmp/expected"

: >"$tmp/env"
env -i PATH="$PATH" "${assignments[@]}" UNRELATED=x "$writer" "$tmp/env" >/dev/null
names_of "$tmp/env" >"$tmp/actual"
diff -u "$tmp/expected" "$tmp/actual" || {
    echo "the written names differ from staging.env.example" >&2
    exit 1
}
grep -qx 'POSTGRES_PASSWORD=secret-value-of-POSTGRES_PASSWORD' "$tmp/env" || {
    echo "a value was not written verbatim" >&2
    exit 1
}
[[ "$(stat -c %a "$tmp/env")" == 600 ]] || { echo "the env file is not mode 0600" >&2; exit 1; }

# A missing required name fails, names it, and prints no value.
: >"$tmp/partial"
if env -i PATH="$PATH" "${assignments[@]}" TOPUP_APP_PASSWORD= "$writer" "$tmp/partial" \
    >"$tmp/out" 2>&1; then
    echo "write-staging-env.sh accepted an empty TOPUP_APP_PASSWORD" >&2
    exit 1
fi
grep -q 'missing or empty: TOPUP_APP_PASSWORD' "$tmp/out" || {
    echo "unexpected failure output:" >&2
    cat "$tmp/out" >&2
    exit 1
}
if grep -q 'secret-value-of' "$tmp/out" "$tmp/partial"; then
    echo "write-staging-env.sh printed or wrote a value on failure" >&2
    exit 1
fi

# Quotes, backticks, and # are refused, naming only the variable.
for bad in 'a#b' 'a"b' "a'b" 'a`b' $'a\nb'; do
    : >"$tmp/bad"
    if env -i PATH="$PATH" "${assignments[@]}" AWS_SECRET_ACCESS_KEY="secret-value-$bad" \
        "$writer" "$tmp/bad" >"$tmp/out" 2>&1; then
        echo "write-staging-env.sh accepted a value containing ${bad:1:1}" >&2
        exit 1
    fi
    grep -q 'without quotes, backticks, or #: AWS_SECRET_ACCESS_KEY$' "$tmp/out" || {
        echo "unexpected failure output:" >&2
        cat "$tmp/out" >&2
        exit 1
    }
    if grep -q 'secret-value' "$tmp/out" "$tmp/bad"; then
        echo "write-staging-env.sh printed or wrote a value on failure" >&2
        exit 1
    fi
done

echo "write-staging-env.sh test passed"
