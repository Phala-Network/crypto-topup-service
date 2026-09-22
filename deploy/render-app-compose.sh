#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
env_file=${1:-"$root/deploy/staging.env.example"}
compose=${2:-"$root/deploy/docker-compose.yml"}
template=${3:-"$root/deploy/app-compose.example.json"}
rendered_compose=$(mktemp)
env_names=$(mktemp)

cleanup() {
    rm -f "$rendered_compose" "$env_names"
}
trap cleanup EXIT INT TERM

"$root/deploy/render-compose.sh" "$compose" >"$rendered_compose"

awk '
/^[[:space:]]*($|#)/ { next }
{
    line = $0
    sub(/^[[:space:]]*export[[:space:]]+/, "", line)
    if (line !~ /^[A-Za-z_][A-Za-z0-9_]*=/) {
        print "invalid env assignment at line " NR > "/dev/stderr"
        exit 64
    }
    sub(/=.*/, "", line)
    if (!seen[line]++) print line
}
' "$env_file" >"$env_names"

allowed_envs=$(jq -Rsc 'split("\n") | map(select(length > 0))' "$env_names")

jq --argjson allowed_envs "$allowed_envs" \
    --rawfile docker_compose_file "$rendered_compose" \
    '.allowed_envs = $allowed_envs | .docker_compose_file = $docker_compose_file' \
    "$template"
