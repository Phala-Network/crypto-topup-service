#!/usr/bin/env bash
# write-staging-env.sh writes exactly the staging.env.example names, writes the owner-sealed
# secrets empty even when set, refuses a missing required name or a value the CLI's dotenv parser
# would alter without printing any value, and accepts empty optional names.
set -euo pipefail

root="$(CDPATH= cd -- "$(dirname -- "$0")/../.." && pwd)"
writer="$root/deploy/write-staging-env.sh"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT INT TERM

names_of() {
    awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); print }' "$1" | sort
}
names_of "$root/deploy/staging.env.example" >"$tmp/expected"

# Every name set to a distinctive value, including the owner-sealed secrets; AWS_ENDPOINT empty.
declare -a assignments=()
while IFS= read -r name; do
    case "$name" in
        AWS_ENDPOINT) assignments+=("$name=") ;;
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
grep -qx 'WALG_S3_PREFIX=secret-value-of-WALG_S3_PREFIX' "$tmp/env" || {
    echo "a value was not written verbatim" >&2
    exit 1
}
for name in AWS_ACCESS_KEY_ID AWS_SECRET_ACCESS_KEY AWS_SESSION_TOKEN COINMETRICS_API_KEY SENTRY_DSN; do
    grep -qx "$name=" "$tmp/env" || {
        echo "the owner-sealed $name was not written empty" >&2
        exit 1
    }
done
[[ "$(stat -c %a "$tmp/env")" == 600 ]] || { echo "the env file is not mode 0600" >&2; exit 1; }

# A missing required name fails, names it, and prints no value.
: >"$tmp/partial"
if env -i PATH="$PATH" "${assignments[@]}" TOPUP_ADMIN_PUBLIC_KEY= "$writer" "$tmp/partial" \
    >"$tmp/out" 2>&1; then
    echo "write-staging-env.sh accepted an empty TOPUP_ADMIN_PUBLIC_KEY" >&2
    exit 1
fi
grep -q 'missing or empty: TOPUP_ADMIN_PUBLIC_KEY' "$tmp/out" || {
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
    if env -i PATH="$PATH" "${assignments[@]}" TOPUP_ADMIN_KID="secret-value-$bad" \
        "$writer" "$tmp/bad" >"$tmp/out" 2>&1; then
        echo "write-staging-env.sh accepted a value containing ${bad:1:1}" >&2
        exit 1
    fi
    grep -q 'without quotes, backticks, or #: TOPUP_ADMIN_KID$' "$tmp/out" || {
        echo "unexpected failure output:" >&2
        cat "$tmp/out" >&2
        exit 1
    }
    if grep -q 'secret-value' "$tmp/out" "$tmp/bad"; then
        echo "write-staging-env.sh printed or wrote a value on failure" >&2
        exit 1
    fi
done

# --product: the product's only name, its signing seed, always empty.
names_of "$root/deploy/product/staging.env.example" >"$tmp/product-expected"
product=()
while IFS= read -r name; do
    product+=("$name=secret-value-of-$name")
done <"$tmp/product-expected"
: >"$tmp/product.env"
env -i PATH="$PATH" "${product[@]}" "$writer" --product "$tmp/product.env" >/dev/null
names_of "$tmp/product.env" | diff -u "$tmp/product-expected" - || {
    echo "--product wrote other names than deploy/product/staging.env.example" >&2
    exit 1
}
grep -qx 'PRODUCT_SEED=' "$tmp/product.env" && ! grep -q 'secret-value-of-PRODUCT_SEED' "$tmp/product.env" || {
    echo "--product wrote the product seed" >&2
    exit 1
}

echo "write-staging-env.sh test passed"
