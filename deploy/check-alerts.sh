#!/bin/sh
# Checks deploy/alerts with promtool from the pinned Prometheus image. Files are copied into the
# container instead of bind-mounted so this also works when the Docker daemon cannot see the
# caller's filesystem (a containerized CI runner using the host socket).
set -eu

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
image=prom/prometheus:v3.14.0@sha256:5ce7540c3c00ef4ab0c9d2c995c6a5b9c421f44b4a115d97a2c7af3b1c21cbb0
container=

cleanup() {
    if [ -n "$container" ]; then
        docker rm -f "$container" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT
trap 'cleanup; exit 130' INT TERM

promtool() {
    container=$(docker create --entrypoint promtool --workdir /rules "$image" "$@")
    docker cp "$root/deploy/alerts/." "$container:/rules"
    docker start --attach "$container"
    docker rm "$container" >/dev/null
    container=
}

promtool check rules prometheus-rules.yml
promtool test rules prometheus-rules.test.yml
