#!/usr/bin/env bash
# Runs the end-to-end tests with Chromium served from Playwright's official image, for hosts
# without Chromium's system libraries (the self-hosted CI runner). The tests, Vite, and Anvil run
# here; the browser container shares this host's network namespace, or this container's when this
# runs inside one, so both sides reach each other on 127.0.0.1. Extra arguments go to
# `playwright test`.
set -euo pipefail
cd "$(dirname "$0")/.."

image="mcr.microsoft.com/playwright:v1.63.0-noble@sha256:eff16c30e6f3f4af0a03fa4b706120d5e9b0891c344a27d64559aff5900a4a27"
version="$(node -p 'require("@playwright/test/package.json").version')"
if [[ $image != *":v$version-"* ]]; then
  echo "@playwright/test $version does not match the browser image $image" >&2
  exit 1
fi
network=host
if [[ -f /.dockerenv ]]; then
  network="container:$(hostname)"
fi
port="${PLAYWRIGHT_SERVER_PORT:-3799}"
name="crypto-topup-playwright-$$"

docker run --detach --rm --init --name "$name" --network "$network" \
  --user pwuser --workdir /home/pwuser "$image" \
  npx -y "playwright@$version" run-server --port "$port" --host 127.0.0.1 >/dev/null
trap 'docker rm --force "$name" >/dev/null 2>&1 || true' EXIT

for _ in $(seq 120); do
  if docker logs "$name" 2>&1 | grep -q "Listening on"; then
    break
  fi
  sleep 1
done
docker logs "$name" 2>&1 | grep -q "Listening on" || { docker logs "$name" >&2; exit 1; }

PLAYWRIGHT_WS_ENDPOINT="ws://127.0.0.1:$port/" pnpm exec playwright test "$@"
