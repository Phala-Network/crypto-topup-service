#!/bin/sh
# Checks deploy/alerts with promtool from the pinned Prometheus image. Files are copied into the
# container instead of bind-mounted so this also works when the Docker daemon cannot see the
# caller's filesystem (a containerized CI runner using the host socket).
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
image=prom/prometheus@sha256:63805ebb8d2b3920190daf1cb14a60871b16fd38bed42b857a3182bc621f4996
container=

cleanup() {
    if [ -n "$container" ]; then
        docker rm -f "$container" >/dev/null 2>&1 || true
    fi
}
trap cleanup EXIT INT TERM

promtool() {
    container=$(docker create --entrypoint promtool --workdir /rules "$image" "$@")
    docker cp "$root/deploy/alerts/." "$container:/rules"
    docker start --attach "$container"
    docker rm "$container" >/dev/null
    container=
}

promtool check rules prometheus-rules.yml
promtool test rules prometheus-rules.test.yml
