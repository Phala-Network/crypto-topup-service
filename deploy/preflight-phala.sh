#!/usr/bin/env bash
# shellcheck disable=SC2154  # tmp, like fail and ok, comes from the sourcing script
# Online preflight checks shared by deploy/preflight.sh and deploy/product/preflight.sh; sourced.
# The caller defines fail, ok, and tmp (a private directory), and sources contracts/common.sh.

# check_anonymous_pulls FILE: every image named in FILE (one per line) pulls without credentials.
check_anonymous_pulls() {
    local docker_host image
    echo "== images (anonymous pull)"
    # `docker pull` of a digest always asks the registry, even when the daemon has the image cached;
    # an empty client config sends no credentials, as the CVM does.
    # The empty config also drops the current Docker context, so keep its daemon endpoint.
    docker_host=${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null)} ||
        docker_host=""
    mkdir "$tmp/docker-anonymous"
    while IFS= read -r image; do
        if DOCKER_HOST=${docker_host:-unix:///var/run/docker.sock} DOCKER_CONFIG="$tmp/docker-anonymous" \
            docker pull --quiet --platform linux/amd64 "$image" >/dev/null 2>&1; then
            ok "$image pulls anonymously"
        else
            fail "$image cannot be pulled anonymously; make the package public"
        fi
    done <"$1"
}

# check_phala_cloud WORKSPACE OS_IMAGE: the CLI version, the logged-in workspace, and a production
# OS image offered by a node of the workspace.
check_phala_cloud() {
    local workspace=$1 os_image=$2 phala version current count offering
    echo "== Phala Cloud (read-only)"
    read -r -a phala <<<"${PHALA:-npx --yes phala@1.1.22}"
    version=$("${phala[@]}" --version 2>/dev/null) || version=error
    [[ "$version" == v1.1.22* || "$version" == 1.1.22* ]] ||
        fail "the Phala CLI is $version; these steps are verified against 1.1.22"
    # Best effort: `status --json` in CLI 1.1.22 reports only the workspace display name (team_name),
    # no workspace id, so two workspaces with the same name cannot be told apart here.
    if "${phala[@]}" status --json >"$tmp/status.json" 2>/dev/null &&
        jq -e --arg workspace "$workspace" '.team_name == $workspace' "$tmp/status.json" >/dev/null; then
        ok "logged in to a workspace named $workspace (display name; best effort)"
    else
        current=$(jq -r '.team_name // empty' "$tmp/status.json" 2>/dev/null) || current=""
        fail "the CLI is not logged in to workspace '$workspace' (current: ${current:-not logged in})"
    fi
    if "${phala[@]}" os-images --prod --all --json >"$tmp/os-images.json" 2>/dev/null &&
        jq -e --arg image "$os_image" \
            'any(.items[]; .name == $image and .is_dev == false and (.version | test("^v?0[.]5[.]9$")))' \
            "$tmp/os-images.json" >/dev/null; then
        ok "OS image $os_image is a production (non-dev) dstack 0.5.9 image"
    else
        fail "OS image $os_image is not listed as a production dstack 0.5.9 image by 'os-images --prod'"
    fi
    # The platform picks the node; it must be one whose images include this one.
    offering='[.nodes[] | select(any(.images[]; .name == $image and .is_dev == false and .version[0:3] == [0, 5, 9]))]'
    if "${phala[@]}" api /teepods/available >"$tmp/nodes.json" 2>/dev/null &&
        count=$(jq -er --arg image "$os_image" "$offering | length" "$tmp/nodes.json") && ((count > 0)); then
        ok "$count node(s) of the workspace offer OS image $os_image"
    else
        fail "no node of the workspace offers OS image $os_image; offered:" \
            "$(jq -r '[.nodes[].images[] | select(.is_dev == false) | .name] | unique | join(", ")' \
                "$tmp/nodes.json" 2>/dev/null)"
    fi
}
