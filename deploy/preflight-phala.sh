#!/usr/bin/env bash
# shellcheck disable=SC2154  # tmp, like fail and ok, comes from the sourcing script
# Preflight checks shared by deploy/preflight.sh and deploy/product/preflight.sh; sourced.
# The caller defines fail, ok, and tmp (a private directory), and sources contracts/common.sh.

# tool_error FILE: the last lines of a tool's stderr, on one line, for a failure message.
tool_error() {
    tail -n 3 "$1" | paste -sd ' ' -
}

# embeds_key URL: whether URL looks like it carries a credential, which an attested setting would
# publish: user info, or a path segment or query value of 20 or more letters, digits, - and _
# with a digit among them, the shape of the API keys of Alchemy, Infura, QuickNode, and Ankr.
embeds_key() {
    local rest=${1#*://} host
    host=${rest%%[/?#]*}
    [[ "$host" == *@* ]] && return 0
    tr '/?&=#;' '\n' <<<"${rest:${#host}}" | grep -E '^[A-Za-z0-9_-]{20,}$' | grep -q '[0-9]'
}

# check_anonymous_pulls FILE: every image named in FILE (one per line) pulls without credentials.
# Every failure is retried with backoff, since registries also refuse transiently; the message
# names the package as private only when the last error is a refusal.
check_anonymous_pulls() {
    local docker_host image attempt
    echo "== images (anonymous pull)"
    # `docker pull` of a digest always asks the registry, even when the daemon has the image cached;
    # an empty client config sends no credentials, as the CVM does, so its errors hold no secret.
    # The empty config also drops the current Docker context, so keep its daemon endpoint.
    docker_host=${DOCKER_HOST:-$(docker context inspect --format '{{.Endpoints.docker.Host}}' 2>/dev/null)} ||
        docker_host=""
    mkdir "$tmp/docker-anonymous"
    while IFS= read -r image; do
        for attempt in 1 2 3; do
            if DOCKER_HOST=${docker_host:-unix:///var/run/docker.sock} DOCKER_CONFIG="$tmp/docker-anonymous" \
                docker pull --quiet --platform linux/amd64 "$image" >/dev/null 2>"$tmp/pull.err" </dev/null; then
                ok "$image pulls anonymously"
                continue 2
            fi
            ((attempt == 3)) || sleep $((attempt * 10))
        done
        if grep -Eiq 'unauthorized|denied|not found|manifest unknown' "$tmp/pull.err"; then
            fail "$image cannot be pulled anonymously; make the package public: $(tool_error "$tmp/pull.err")"
        else
            fail "$image did not pull in 3 attempts: $(tool_error "$tmp/pull.err")"
        fi
    done <"$1"
}

# check_phala_cloud WORKSPACE OS_IMAGE: the CLI version, the logged-in workspace, and a production
# OS image offered by a node of the workspace.
check_phala_cloud() {
    local workspace=$1 os_image=$2 phala version current count offering
    echo "== Phala Cloud (read-only)"
    read -r -a phala <<<"${PHALA:-npx --yes phala@1.1.22}"
    version=$("${phala[@]}" --version 2>"$tmp/phala.err") || version="error: $(tool_error "$tmp/phala.err")"
    [[ "$version" == v1.1.22* || "$version" == 1.1.22* ]] ||
        fail "the Phala CLI is $version; these steps are verified against 1.1.22"
    # Best effort: `status --json` in CLI 1.1.22 reports only the workspace display name (team_name),
    # no workspace id, so two workspaces with the same name cannot be told apart here.
    if ! "${phala[@]}" status --json >"$tmp/status.json" 2>"$tmp/phala.err"; then
        fail "the CLI is not logged in to workspace '$workspace': $(tool_error "$tmp/phala.err")"
    elif jq -e --arg workspace "$workspace" '.team_name == $workspace' "$tmp/status.json" >/dev/null; then
        ok "logged in to a workspace named $workspace (display name; best effort)"
    else
        current=$(jq -r '.team_name // empty' "$tmp/status.json" 2>/dev/null) || current=""
        fail "the CLI is not logged in to workspace '$workspace' (current: ${current:-not logged in})"
    fi
    if ! "${phala[@]}" os-images --prod --all --json >"$tmp/os-images.json" 2>"$tmp/phala.err"; then
        fail "'os-images --prod' failed: $(tool_error "$tmp/phala.err")"
    elif jq -e --arg image "$os_image" \
        'any(.items[]; .name == $image and .is_dev == false and (.version | test("^v?0[.]5[.]9$")))' \
        "$tmp/os-images.json" >/dev/null; then
        ok "OS image $os_image is a production (non-dev) dstack 0.5.9 image"
    else
        fail "OS image $os_image is not listed as a production dstack 0.5.9 image by 'os-images --prod'"
    fi
    # The platform picks the node; it must be one whose images include this one.
    offering='[.nodes[] | select(any(.images[]; .name == $image and .is_dev == false and .version[0:3] == [0, 5, 9]))]'
    if ! "${phala[@]}" api /teepods/available >"$tmp/nodes.json" 2>"$tmp/phala.err"; then
        fail "'api /teepods/available' failed: $(tool_error "$tmp/phala.err")"
    elif count=$(jq -er --arg image "$os_image" "$offering | length" "$tmp/nodes.json") && ((count > 0)); then
        ok "$count node(s) of the workspace offer OS image $os_image"
    else
        fail "no node of the workspace offers OS image $os_image; offered:" \
            "$(jq -r '[.nodes[].images[] | select(.is_dev == false) | .name] | unique | join(", ")' \
                "$tmp/nodes.json" 2>/dev/null)"
    fi
}
