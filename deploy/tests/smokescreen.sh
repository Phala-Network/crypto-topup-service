#!/usr/bin/env bash
# Starts the attested compose's smokescreen sidecar from IMAGE (the phala-pay image, which carries
# the binary) with the compose's own command, and proves its policy through the proxy, as the
# service's webhook client uses it: plain HTTP requests and CONNECT tunnels to private, loopback,
# link-local and cloud metadata, CGNAT, 0.0.0.0/8, IPv4-mapped IPv6, NAT64, and unique-local
# addresses, and to a name that resolves to loopback, are refused with 407 before any connection;
# a public address passes the IP filter and fails only to connect (TEST-NET-3 routes nowhere). No
# network access beyond the loopback is needed.
#
# Usage: deploy/tests/smokescreen.sh IMAGE
set -euo pipefail

image=${1:?usage: deploy/tests/smokescreen.sh IMAGE}
root="$(CDPATH='' cd -- "$(dirname -- "$0")/../.." && pwd)"
name="phala-pay-smokescreen-test-$$"
cleanup() {
    docker rm -f "$name" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

# The command is literal in the compose source; rendering substitutes only variables.
mapfile -t command < <(docker compose -f "$root/deploy/docker-compose.yml" config --format json \
    2>/dev/null | jq -er '.services.smokescreen.command[]')
[[ "${command[0]}" == smokescreen ]] || { echo "the compose does not run smokescreen" >&2; exit 1; }

docker run -d --name "$name" -p 127.0.0.1::4750 "$image" "${command[@]}" >/dev/null
port=$(docker port "$name" 4750/tcp | head -n 1 | sed 's/.*://')
proxy="http://127.0.0.1:$port"
for _ in $(seq 50); do
    if curl -s -o /dev/null --max-time 1 "$proxy/"; then
        break
    fi
    sleep 0.2
done

failures=0
# $1: `http` (an absolute-form request) or `connect` (a CONNECT tunnel); $2: the target URL.
status() {
    if [[ "$1" == connect ]]; then
        curl -s -o /dev/null --max-time 20 -w '%{http_connect}' --proxytunnel -x "$proxy" "$2" || true
    else
        curl -s -o /dev/null --max-time 20 -w '%{http_code}' -x "$proxy" "$2" || true
    fi
}
expect_refused() {
    local code
    code=$(status "$1" "$2")
    if [[ "$code" == 407 ]]; then
        echo "ok: refused $1 $2"
    else
        echo "FAIL: $1 $2 answered $code, not 407" >&2
        failures=$((failures + 1))
    fi
}

for mode in http connect; do
    for target in \
        http://10.0.0.1/ http://172.16.0.1/ http://192.168.1.1/ \
        http://127.0.0.1:4750/ http://localhost/ 'http://[::1]/' \
        http://169.254.169.254/latest/meta-data/ http://100.100.100.200/latest/meta-data/ \
        http://100.64.0.1/ http://100.127.255.254/ \
        http://0.1.2.3/ \
        'http://[::ffff:10.0.0.1]/' 'http://[::ffff:127.0.0.1]/' \
        'http://[::ffff:169.254.169.254]/' 'http://[::ffff:100.64.0.1]/' \
        'http://[64:ff9b::a00:1]/' 'http://[fd00:ec2::254]/' 'http://[fe80::1]/'; do
        expect_refused "$mode" "$target"
    done
done

# A public address passes the filter: smokescreen tries to connect and reports that it could not.
code=$(status http http://203.0.113.10/)
if [[ "$code" == 502 || "$code" == 504 ]]; then
    echo "ok: allowed a public address (connection failed with $code, as TEST-NET-3 routes nowhere)"
else
    echo "FAIL: a public address answered $code, not 502 or 504" >&2
    failures=$((failures + 1))
fi

if ((failures > 0)); then
    docker logs "$name" 2>&1 | tail -n 40 >&2
    echo "smokescreen policy test failed: $failures checks" >&2
    exit 1
fi
echo "smokescreen policy test passed"
