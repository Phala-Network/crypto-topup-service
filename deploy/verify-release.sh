#!/usr/bin/env bash
# Verifies release VERSION of Phala-Network/phala-pay (deploy/README.md, "Releases") and leaves its
# assets in DIR; prints the release's commit. Deploy runs it, and so does an operator by hand. It
# stops at the first failure:
#   1. The release's commit is the one its tag names (a protected tag of an immutable release), and
#      a commit of main's history.
#   2. Every asset matches SHA256SUMS.
#   3. Every asset, and every image images.json names, has a GitHub build provenance attestation
#      signed by release.yml at refs/tags/VERSION on a GitHub-hosted runner, for that commit.
#
# It needs the GitHub CLI 2.101.0 (the version Deploy pins), logged in or with GH_TOKEN, and jq.
#
# Usage: verify-release.sh VERSION DIR
set -euo pipefail

repository=Phala-Network/phala-pay
version=${1:-} dir=${2:-}
[[ "$version" =~ ^v(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z.-]+)?$ && -n "$dir" ]] ||
    { echo "usage: $0 VERSION DIR" >&2; exit 64; }

commit=$(gh api "repos/$repository/commits/refs/tags/$version" --jq .sha)
[[ "$(gh api "repos/$repository/compare/$commit...main" --jq .status)" =~ ^(ahead|identical)$ ]] ||
    { echo "$version's commit $commit is not in $repository's main" >&2; exit 1; }
echo "$version is commit $commit, in main's history" >&2

mkdir -p "$dir"
gh release download "$version" -R "$repository" -D "$dir" --clobber
(cd "$dir" && if command -v sha256sum >/dev/null; then sha256sum --quiet -c SHA256SUMS; else
    shasum -a 256 --quiet -c SHA256SUMS; fi)

provenance=(-R "$repository" --source-digest "$commit" --deny-self-hosted-runners
    --cert-identity "https://github.com/$repository/.github/workflows/release.yml@refs/tags/$version")
for asset in images.json "phala-pay-deploy-$version.tar.gz" phala-cloud-template.yml; do
    gh attestation verify "$dir/$asset" "${provenance[@]}" >/dev/null
    echo "verified $asset" >&2
done
while IFS= read -r image; do
    [[ "$image" =~ ^[a-z0-9./_-]+@sha256:[0-9a-f]{64}$ ]] || { echo "not a digest: $image" >&2; exit 1; }
    gh attestation verify "oci://$image" "${provenance[@]}" >/dev/null
    echo "verified $image" >&2
done < <(jq -r '.[]' "$dir/images.json")
echo "$commit"
