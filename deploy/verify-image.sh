#!/bin/sh
# Builds an image of this checkout (DOCKERFILE, default the phala-pay Dockerfile) for PLATFORM, with
# the commit's time as SOURCE_DATE_EPOCH and the commit as SOURCE_COMMIT, exactly as the Release
# workflow does: Buildx v0.37.1 and a temporary builder running the pinned BuildKit image.
#
# By default the image must be reproducible: build 1 goes to an OCI archive, build 2 to another
# archive or, with PUBLISH_IMAGE (a repository:tag), to the registry, and both must have the same
# manifest and config digests; the registry must serve the manifest built. --once builds once and
# publishes: postgres-walg, which is not reproducible (apt and dpkg record wall-clock times) and so
# has provenance only. IMAGE_REF_FILE receives the published repository@sha256.
#
# Usage: deploy/verify-image.sh [--once]
set -eu

buildx=v0.37.1
buildkit=moby/buildkit:v0.33.0@sha256:6c2fa84a6b61ccd72899dde4239f8d5717f05f9a8ca6f3cad185fb1a95a94de3

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
# The commit compiled in as the Sentry release (Dockerfile).
source_commit=${SOURCE_COMMIT:-$(git -C "$root" rev-parse HEAD)}
platform=${PLATFORM:-linux/amd64}
dockerfile="$root/${DOCKERFILE:-Dockerfile}"
once=0
case "${1:-}" in
    --once) once=1 ;;
    '') ;;
    *) echo "usage: $0 [--once]" >&2; exit 64 ;;
esac
case "${PUBLISH_IMAGE:-}" in
    '') [ "$once" -eq 0 ] || { echo "--once publishes: set PUBLISH_IMAGE" >&2; exit 64; } ;;
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

# build OUTPUT: one clean build to OUTPUT (a Buildx --output).
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
        --output "$1,rewrite-timestamp=true,oci-mediatypes=true" \
        "$root" >&2
}
# archived N: build N as an OCI archive; prints its manifest and config digests.
archived() {
    build "type=oci,dest=$tmp/build-$1.tar,name=phala-pay:repro"
    mkdir "$tmp/build-$1"
    tar -xf "$tmp/build-$1.tar" -C "$tmp/build-$1"
    manifest=$(jq -er '.manifests[0].digest' "$tmp/build-$1/index.json")
    printf '%s %s\n' "$manifest" "$(jq -er '.config.digest' "$tmp/build-$1/blobs/sha256/${manifest#sha256:}")"
}
# published: build and push PUBLISH_IMAGE; prints the manifest and config digests the registry
# serves (one platform and no attestations: a manifest, not an index).
published() {
    build "type=image,name=$PUBLISH_IMAGE,push=true,unpack=false"
    docker buildx imagetools inspect "$PUBLISH_IMAGE" --raw >"$tmp/registry.json"
    printf 'sha256:%s %s\n' "$(sha256sum "$tmp/registry.json" | awk '{print $1}')" \
        "$(jq -er '.config.digest' "$tmp/registry.json")"
}

if [ "$once" -eq 1 ]; then
    built=$(published)
    echo "published (not reproducible): manifest=${built% *} config=${built#* }"
else
    first=$(archived 1)
    if [ -n "${PUBLISH_IMAGE:-}" ]; then second=$(published); else second=$(archived 2); fi
    printf 'build 1: manifest=%s config=%s\nbuild 2: manifest=%s config=%s\n' \
        "${first% *}" "${first#* }" "${second% *}" "${second#* }"
    [ "$first" = "$second" ] || { echo "image reproducibility check failed" >&2; exit 1; }
    echo "image reproducibility check passed"
    built=$second
fi
if [ -n "${PUBLISH_IMAGE:-}" ] && [ -n "${IMAGE_REF_FILE:-}" ]; then
    printf '%s@%s\n' "${PUBLISH_IMAGE%:*}" "${built% *}" >"$IMAGE_REF_FILE"
fi
