#!/bin/sh
# Builds an image of this checkout (DOCKERFILE, default the phala-pay Dockerfile) for PLATFORM, with
# the commit's time as SOURCE_DATE_EPOCH and the commit as SOURCE_COMMIT, exactly as the Release
# workflow does: Buildx v0.37.1 and a temporary builder running the pinned BuildKit image.
#
# Every digest is the one BuildKit reports for what it built (`--metadata-file`,
# `containerimage.digest` and `containerimage.config.digest`), never a tag read back. By default the
# image must be reproducible: build 1 goes to an OCI archive, build 2 to another archive or, with
# PUBLISH_IMAGE (a repository:tag), to the registry, and both must have the same manifest and config
# digests. IMAGE_REF_FILE receives
# repository@<the pushed build's digest>, which the Release workflow smoke-tests and attests.
#
# Usage: deploy/verify-image.sh
set -eu

buildx=v0.37.1
buildkit=moby/buildkit:v0.33.0@sha256:6c2fa84a6b61ccd72899dde4239f8d5717f05f9a8ca6f3cad185fb1a95a94de3

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
# The commit compiled in as the Sentry release (Dockerfile).
source_commit=${SOURCE_COMMIT:-$(git -C "$root" rev-parse HEAD)}
platform=${PLATFORM:-linux/amd64}
dockerfile="$root/${DOCKERFILE:-Dockerfile}"
[ "$#" -eq 0 ] || { echo "usage: $0" >&2; exit 64; }
case "${PUBLISH_IMAGE:-}" in
    '') ;;
    *@sha256:*) echo "PUBLISH_IMAGE must be a repository:tag, not a digest" >&2; exit 64 ;;
    *) case "${PUBLISH_IMAGE##*/}" in
        *:*) ;;
        *) echo "PUBLISH_IMAGE must include a tag" >&2; exit 64 ;;
    esac ;;
esac
docker buildx version | grep -q " $buildx " ||
    { echo "Buildx $buildx is required (docker buildx version)" >&2; exit 1; }

tmp=$(mktemp -d)
builder="phala-pay-verify-$$"
cleanup() {
    docker buildx rm "$builder" >/dev/null 2>&1 || true
    rm -rf "$tmp"
}
trap cleanup EXIT INT TERM
docker buildx create --name "$builder" --driver docker-container --driver-opt "image=$buildkit" \
    >/dev/null

# SBOM and provenance are signed separately for the final digest after comparison; embedding
# build-time attestations here would make the comparison nondeterministic.
# build N OUTPUT: clean build N to OUTPUT (a Buildx --output); prints the manifest and config
# digests BuildKit reports for it.
build() {
    docker buildx build \
        --builder "$builder" \
        --no-cache \
        --platform "$platform" \
        --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch" \
        --build-arg "SOURCE_COMMIT=$source_commit" \
        --provenance=false \
        --sbom=false \
        --file "$dockerfile" \
        --metadata-file "$tmp/build-$1.json" \
        --output "$2,rewrite-timestamp=true,oci-mediatypes=true" \
        "$root" >&2
    jq -er '"\(."containerimage.digest") \(."containerimage.config.digest")"' "$tmp/build-$1.json"
}
archived() {
    build "$1" "type=oci,dest=$tmp/build-$1.tar,name=phala-pay:repro"
}
published() {
    build "$1" "type=image,name=$PUBLISH_IMAGE,push=true,unpack=false"
}

first=$(archived 1)
if [ -n "${PUBLISH_IMAGE:-}" ]; then second=$(published 2); else second=$(archived 2); fi
printf 'build 1: manifest=%s config=%s\nbuild 2: manifest=%s config=%s\n' \
    "${first% *}" "${first#* }" "${second% *}" "${second#* }"
[ "$first" = "$second" ] || { echo "image reproducibility check failed" >&2; exit 1; }
echo "image reproducibility check passed"
built=$second
if [ -n "${PUBLISH_IMAGE:-}" ] && [ -n "${IMAGE_REF_FILE:-}" ]; then
    printf '%s@%s\n' "${PUBLISH_IMAGE%:*}" "${built% *}" >"$IMAGE_REF_FILE"
fi
