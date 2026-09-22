#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
template=${1:-"$root/deploy/app-compose.example.json"}
compose=${2:-"$root/deploy/docker-compose.yml"}

jq --rawfile docker_compose_file "$compose" \
    '.docker_compose_file = $docker_compose_file' "$template"
