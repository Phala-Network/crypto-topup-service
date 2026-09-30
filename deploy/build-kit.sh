#!/usr/bin/env bash
# Builds the deploy kit of a release (deploy/README.md, "Releases"): `git archive` of COMMIT's
# (default HEAD's) LICENSE, deploy/, and docs/ under `phala-pay-deploy-VERSION/`, as
# OUT_DIR/phala-pay-deploy-VERSION.tar.gz. Its tar is identical to that `git archive` from the tag;
# the gzip layer depends on the compressor. It holds no image; the release's images.json names them.
#
# Usage: deploy/build-kit.sh VERSION OUT_DIR [COMMIT]
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
version=${1:-} out=${2:-} commit=${3:-HEAD}
[[ "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ && -n "$out" ]] ||
    { echo "usage: $0 VERSION OUT_DIR [COMMIT]" >&2; exit 64; }
name=phala-pay-deploy-$version
mkdir -p "$out"
git -C "$root" archive --format=tar --prefix="$name/" "$commit" -- LICENSE deploy docs |
    gzip -9n >"$out/$name.tar.gz.partial"
mv "$out/$name.tar.gz.partial" "$out/$name.tar.gz"
echo "$out/$name.tar.gz"
