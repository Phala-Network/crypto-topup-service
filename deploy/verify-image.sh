#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
source_date_epoch=${SOURCE_DATE_EPOCH:-$(git -C "$root" log -1 --pretty=%ct)}
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
        --build-arg "SOURCE_DATE_EPOCH=$source_date_epoch" \
        --provenance=false \
        --sbom=false \
        --output "type=oci,dest=$archive,name=crypto-topup-service:repro,rewrite-timestamp=true" \
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
