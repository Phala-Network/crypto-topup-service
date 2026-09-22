#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)

if [ "$#" -eq 1 ] && jq -e 'type == "object"' "$1" >/dev/null 2>&1; then
    jq -cjS . "$1"
else
    "$root/deploy/render-app-compose.sh" "$@" | jq -cjS .
fi | sha256sum | awk '{print $1}'
