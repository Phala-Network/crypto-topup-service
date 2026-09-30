#!/usr/bin/env bash
# deploy/verify-release.sh against a stub `gh`: a good release verifies every asset, SHA256SUMS,
# and each image, and prints its commit; it stops, before any image, on an images.json that is not
# the three images by digest, and it fails on a commit outside main, a checksum mismatch, or a
# refused attestation.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
commit=$(printf 'c%.0s' {1..40})
mkdir -p "$tmp/bin" "$tmp/assets"
cat >"$tmp/bin/gh" <<'STUB'
#!/usr/bin/env bash
echo "$*" >>"$STUB_LOG"
case "$1 $2" in
    "api repos/Phala-Network/phala-pay/commits/refs/tags/v9.9.9") echo "$STUB_COMMIT" ;;
    "api repos/Phala-Network/phala-pay/compare/$STUB_COMMIT...main") echo "$STUB_COMPARE" ;;
    "release download") cp "$STUB_ASSETS"/* "${@: -2:1}" ;;
    "attestation verify") [[ "$3" != *"$STUB_REFUSE"* ]] ;;
    *) exit 1 ;;
esac
STUB
chmod +x "$tmp/bin/gh"
export PATH="$tmp/bin:$PATH" STUB_LOG="$tmp/log" STUB_ASSETS="$tmp/assets" STUB_COMMIT="$commit"

# assets IMAGES_JSON: the release's four assets, with IMAGES_JSON as images.json.
assets() {
    rm -f "$tmp/assets"/*
    printf '%s\n' "$1" >"$tmp/assets/images.json"
    echo kit >"$tmp/assets/phala-pay-deploy-v9.9.9.tar.gz"
    echo template >"$tmp/assets/phala-cloud-template.yml"
    (cd "$tmp/assets" && sha256sum images.json phala-pay-deploy-v9.9.9.tar.gz phala-cloud-template.yml \
        >SHA256SUMS)
}
digest=$(printf '1%.0s' {1..64})
good="{\"phala-pay\": \"ghcr.io/phala-network/phala-pay@sha256:$digest\",
    \"postgres-walg\": \"ghcr.io/phala-network/postgres-walg@sha256:$digest\",
    \"phala-pay-reference-product\": \"ghcr.io/phala-network/phala-pay-reference-product@sha256:$digest\"}"
# run NAME [ENV...]: verify-release.sh into a fresh directory, with ENV set.
run() {
    local name=$1
    shift
    : >"$STUB_LOG"
    env STUB_COMPARE=ahead STUB_REFUSE=never "$@" "$root/deploy/verify-release.sh" v9.9.9 "$tmp/$name" \
        >"$tmp/$name.out" 2>"$tmp/$name.err"
}
fail() {
    echo "verify-release: $*" >&2
    exit 1
}

assets "$good"
run good || { cat "$tmp/good.err" >&2; fail "refused a good release"; }
[[ "$(cat "$tmp/good.out")" == "$commit" ]] || fail "did not print the release's commit"
[[ "$(grep -c '^attestation verify' "$STUB_LOG")" == 7 ]] || fail "did not verify 4 assets and 3 images"
grep -q "^attestation verify $tmp/good/SHA256SUMS .*--source-digest $commit" "$STUB_LOG" ||
    fail "did not verify SHA256SUMS for the release's commit"

for bad in '{"phala-pay": ' "{\"extra\": \"x@sha256:$digest\", ${good#\{}" '{"phala-pay": "phala-pay:latest"}'; do
    assets "$bad"
    ! run bad || fail "accepted images.json $bad"
    ! grep -q '^attestation verify oci://' "$STUB_LOG" || fail "verified an image of a bad images.json"
done
assets "$good"
! run outside STUB_COMPARE=diverged || fail "accepted a commit outside main"
! grep -q '^release download' "$STUB_LOG" || fail "downloaded a release whose commit is outside main"
! run unattested STUB_REFUSE=SHA256SUMS || fail "accepted an unattested SHA256SUMS"
echo tampered >"$tmp/assets/phala-cloud-template.yml"
! run tampered || fail "accepted an asset that does not match SHA256SUMS"
echo "release verifier test passed"
