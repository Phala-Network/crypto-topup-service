#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
"$root/deploy/render-app-compose.sh" "$@" | jq -cjS . | sha256sum | awk '{print $1}'
