#!/usr/bin/env bash
# Builds the deploy kit of a release (deploy/README.md, "Releases"): the files an operator needs to
# render, check, deploy, verify, and restore an environment directory, at repository paths under
# `phala-pay-deploy-VERSION/`, as OUT_DIR/phala-pay-deploy-VERSION.tar.gz. It holds no image and
# needs no Rust or Solidity toolchain; the release's images.json names the images.
#
# The tar is `git archive` of COMMIT (default HEAD): the kit's tar (`gzip -dc`) is identical to
# `deploy/build-kit.sh --tar VERSION TAG`, byte for byte. The gzip layer is not claimed
# reproducible; compare the tar.
#
# Usage: deploy/build-kit.sh VERSION OUT_DIR [COMMIT]
#        deploy/build-kit.sh --tar VERSION [COMMIT] >kit.tar
set -euo pipefail

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
usage() {
    echo "usage: $0 VERSION OUT_DIR [COMMIT] | $0 --tar VERSION [COMMIT]" >&2
    exit 64
}
tar_only=0
[[ "${1:-}" != --tar ]] || { tar_only=1; shift; }
version=${1:-}
[[ "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ ]] || usage
if ((tar_only)); then
    (($# <= 2)) || usage
    commit=${2:-HEAD}
else
    (($# >= 2 && $# <= 3)) || usage
    out=$2 commit=${3:-HEAD}
fi

paths=(
    LICENSE
    # The attested stack, its variants and policy, and the renderer.
    deploy/compose.yaml
    deploy/compose.service.yaml
    deploy/compose.restore-check.yaml
    deploy/compose.template.yaml
    deploy/compose-policy.jq
    deploy/render.sh
    deploy/pinned-compose.sh
    deploy/postgres-init
    deploy/product/compose.yaml
    # Preflight, the route-mode check, the Phala Cloud CLI wrapper, and the verifiers.
    deploy/preflight.sh
    deploy/preflight-phala.sh
    deploy/product/preflight.sh
    deploy/check-route-modes.sh
    deploy/phala-cvm.sh
    deploy/verify-attestation.sh
    deploy/dstack-verifier.sh
    deploy/verify-ingress-evidence.sh
    deploy/pre-launch-scripts.json
    deploy/contracts/common.sh
    deploy/contracts/verify-deployment.sh
    deploy/contracts/reference.json
    deploy/contracts/networks.json
    deploy/contracts/multicall3.json
    # Environments to start from, and the documentation and runbooks.
    deploy/environments/example
    deploy/environments/phala-cloud-template
    deploy/README.md
    deploy/RESTORE.md
    deploy/CONTRACTS.md
    ':(glob)deploy/runbooks/*.md'
    deploy/runbooks/sign-admin-request.sh
    docs/self-hosting.md
    docs/configuration.md
)

name=phala-pay-deploy-$version
archive() {
    git -C "$root" archive --format=tar --prefix="$name/" "$commit" -- "${paths[@]}"
}
if ((tar_only)); then
    archive
    exit 0
fi
mkdir -p "$out"
archive | gzip -9n >"$out/$name.tar.gz.partial"
mv "$out/$name.tar.gz.partial" "$out/$name.tar.gz"
echo "$out/$name.tar.gz"
