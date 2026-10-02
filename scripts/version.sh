#!/usr/bin/env bash
# Phala Pay's one version: the Cargo workspace version, which the service, @phala/pay (sdk/js), and
# phala-pay (sdk/python) share, all released by one `v<version>` tag (CONTRIBUTING.md, "Releasing").
#
#   scripts/version.sh            prints it, and fails unless sdk/js/package.json,
#                                 sdk/python/pyproject.toml, and sdk/python/uv.lock name it too
#   scripts/version.sh VERSION    sets it in Cargo.toml, Cargo.lock, and those three (needs cargo,
#                                 jq, and uv)
#
# VERSION is X.Y.Z, or X.Y.Z-rc.N for a pre-release, which Python spells X.Y.ZrcN (PEP 440).
set -euo pipefail

cd "$(dirname "$0")/.."

cargo_version() {
    sed -n '/^\[workspace.package\]$/,/^\[/s/^version = "\(.*\)"$/\1/p' Cargo.toml
}
python_version() {
    sed -n '/^\[project\]$/,/^\[/s/^version = "\(.*\)"$/\1/p' sdk/python/pyproject.toml
}
# The version of the lock's own package, phala-pay.
python_lock_version() {
    awk '/^\[\[package\]\]$/ { own = 0 } $0 == "name = \"phala-pay\"" { own = 1 }
        own && /^version = / { gsub(/^version = "|"$/, ""); print; exit }' sdk/python/uv.lock
}

if (($# == 1)); then
    [[ "$1" =~ ^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-rc\.(0|[1-9][0-9]*))?$ ]] ||
        { echo "version.sh: $1 is not X.Y.Z or X.Y.Z-rc.N" >&2; exit 64; }
    sed -i "/^\[workspace.package\]\$/,/^\[/s/^version = \".*\"\$/version = \"$1\"/" Cargo.toml
    cargo update --workspace --quiet
    jq --arg version "$1" '.version = $version' sdk/js/package.json >sdk/js/package.json.tmp
    mv sdk/js/package.json.tmp sdk/js/package.json
    uv version --quiet --project sdk/python --no-sync "$1"
elif (($# != 0)); then
    echo "usage: version.sh [VERSION]" >&2
    exit 64
fi

version=$(cargo_version)
python=${version/-rc./rc}
status=0
mismatch() {
    echo "version.sh: $1 names ${2:-no version}, not the Cargo workspace version $version" >&2
    status=1
}
[[ "$(jq -r .version sdk/js/package.json)" == "$version" ]] ||
    mismatch sdk/js/package.json "$(jq -r .version sdk/js/package.json)"
[[ "$(python_version)" == "$python" ]] || mismatch sdk/python/pyproject.toml "$(python_version)"
[[ "$(python_lock_version)" == "$python" ]] || mismatch sdk/python/uv.lock "$(python_lock_version)"
((status == 0)) || exit 1
echo "$version"
