#!/usr/bin/env bash
# write-staging-env.sh writes exactly the names of the env example, every one empty even when the
# environment sets it, with mode 0600; --product does the same for the reference product.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
writer="$root/deploy/write-staging-env.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

names_of() {
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$1" | sort
}

for variant in staging product; do
    if [[ "$variant" == product ]]; then
        example="$root/deploy/product/staging.env.example" flag=(--product)
    else
        example="$root/deploy/staging.env.example" flag=()
    fi
    names_of "$example" >"$tmp/expected"
    assignments=()
    while IFS= read -r name; do
        assignments+=("$name=secret-value-of-$name")
    done <"$tmp/expected"
    : >"$tmp/env"
    env -i PATH="$PATH" "${assignments[@]}" "$writer" "${flag[@]}" "$tmp/env" >/dev/null
    names_of "$tmp/env" | diff -u "$tmp/expected" - || {
        echo "$variant: the written names differ from $example" >&2
        exit 1
    }
    if grep -v '^[A-Z_][A-Z0-9_]*=$' "$tmp/env" >/dev/null; then
        echo "$variant: a value was written; every name must be empty" >&2
        exit 1
    fi
    [[ "$(stat -c %a "$tmp/env")" == 600 ]] || { echo "the env file is not mode 0600" >&2; exit 1; }
done

echo "write-staging-env.sh test passed"
