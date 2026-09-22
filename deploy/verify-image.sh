#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
platform=${PLATFORM:-linux/amd64}
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -type f -delete
    find "$tmp" -depth -type d -empty -delete
}
trap cleanup EXIT INT TERM

build() {
    number=$1
    archive="$tmp/build-$number.tar"
    output="$tmp/build-$number"
    mkdir -p "$output"
    docker buildx build \
        --no-cache \
        --platform "$platform" \
        --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch" \
        --provenance=false \
        --sbom=false \
        --output "type=oci,dest=$archive,name=crypto-topup-service:repro,rewrite-timestamp=true,oci-mediatypes=true" \
        "$root"
    tar -xf "$archive" -C "$output"

    manifest_digest=$(jq -r '.manifests[0].digest' "$output/index.json")
    manifest_path="$output/blobs/sha256/${manifest_digest#sha256:}"
    config_digest=$(jq -r '.config.digest' "$manifest_path")
    printf '%s %s\n' "$manifest_digest" "$config_digest"
}

first=$(build 1)
second=$(build 2)

printf 'build 1: manifest=%s config=%s\n' $first
printf 'build 2: manifest=%s config=%s\n' $second

if [ "$first" != "$second" ]; then
    echo "image reproducibility check failed" >&2
    exit 1
fi

echo "image reproducibility check passed"

if [ -n "${PUBLISH_IMAGE:-}" ]; then
    case "$PUBLISH_IMAGE" in
        *@sha256:*)
            echo "PUBLISH_IMAGE must be a writable repository tag, not a digest" >&2
            exit 64
            ;;
        *:*) ;;
        *) echo "PUBLISH_IMAGE must include an explicit candidate tag" >&2; exit 64 ;;
    esac

    docker buildx build \
        --no-cache \
        --platform "$platform" \
        --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch" \
        --provenance=false \
        --sbom=false \
        --output "type=image,name=$PUBLISH_IMAGE,push=true,rewrite-timestamp=true,oci-mediatypes=true" \
        "$root"

    registry_raw="$tmp/registry-raw.json"
    registry_manifest="$tmp/registry-manifest.json"
    docker buildx imagetools inspect "$PUBLISH_IMAGE" --raw >"$registry_raw"
    if jq -e '.manifests' "$registry_raw" >/dev/null 2>&1; then
        os=${platform%/*}
        arch=${platform#*/}
        registry_manifest_digest=$(jq -er \
            --arg os "$os" --arg arch "$arch" \
            '.manifests[] | select(.platform.os == $os and .platform.architecture == $arch) | .digest' \
            "$registry_raw")
        docker buildx imagetools inspect \
            "$PUBLISH_IMAGE@$registry_manifest_digest" --raw >"$registry_manifest"
        registry_index_digest="sha256:$(sha256sum "$registry_raw" | awk '{print $1}')"
    else
        cp "$registry_raw" "$registry_manifest"
        registry_manifest_digest="sha256:$(sha256sum "$registry_manifest" | awk '{print $1}')"
        registry_index_digest=$registry_manifest_digest
    fi
    registry_config_digest=$(jq -er '.config.digest' "$registry_manifest")

    printf 'registry: index=%s manifest=%s config=%s\n' \
        "$registry_index_digest" "$registry_manifest_digest" "$registry_config_digest"
    if [ "$registry_manifest_digest $registry_config_digest" != "$first" ]; then
        echo "published image differs from verified local image" >&2
        exit 1
    fi
    echo "published image matches verified local manifest and config"
fi
