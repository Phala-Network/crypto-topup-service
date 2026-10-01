#!/usr/bin/env bash
# The env file convention (deploy/preflight-phala.sh, read_env_file) against the parser of the
# locked Phala Cloud CLI that seals it: each value written as deploy.sh writes it (NAME=VALUE) is
# either refused by preflight, or read back unchanged by both preflight and the CLI. The CLI's
# parser is taken from its own bundle, as installed from deploy/tools' lockfile, so a CLI upgrade
# that changes it fails here.
set -euo pipefail

root=$(CDPATH='' cd -- "$(dirname "$0")/../.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM
fail() {
    echo "env-file: $*" >&2
    exit 1
}

cp "$root/deploy/tools/package.json" "$root/deploy/tools/package-lock.json" "$tmp/"
npm ci --prefix "$tmp" --ignore-scripts --no-audit --no-fund --loglevel=error >/dev/null
# cli_parse FILE: the CLI's reading of FILE as JSON, by the parser its env-file reader calls.
cli_parse() {
    node - "$tmp/node_modules/phala/dist/index.js" "$1" <<'JS'
const fs = require("fs");
const [bundle, file] = process.argv.slice(2);
const source = fs.readFileSync(bundle, "utf8");
const parser = source.match(
    /([\w$]+)=(\/\(\?:\^\|\^\)\\s\*\(\?:export\\s\+\)\?.*?\/gm);function ([\w$]+)\(e\)\{let t=\{\},n=e\.toString\(\);[\s\S]*?return t\}/);
if (!parser) throw new Error("the CLI bundle has no dotenv parser");
const parse = new Function(`let ${parser[0]}; return ${parser[3]};`)();
process.stdout.write(JSON.stringify(parse(fs.readFileSync(file, "utf8"))));
JS
}
# preflight_parse FILE: preflight's reading of FILE as JSON, or nothing when it refuses the file.
preflight_parse() {
    (
        refused=0
        fail() { refused=1; }
        declare -A env=()
        # shellcheck source=deploy/preflight-phala.sh
        source "$root/deploy/preflight-phala.sh"
        read_env_file "$1"
        ((refused)) && exit 0
        for name in "${!env[@]}"; do
            jq -n --arg key "$name" --arg value "${env[$name]}" '{($key): $value}'
        done | jq -cs 'add // {}'
    )
}

accepted=(plain-ABC_123 "a b" "x=y" 'back\slash' 'a\nb' '$HOME' 'https://0123@o1.ingest.sentry.io/42' "")
refused=('alpha#bravo' "'quoted'" '"quoted"' '`ticked`' "it's" ' leading' 'trailing ' $'tab\t')
for value in "${accepted[@]}" "${refused[@]}"; do
    printf '%s=%s\n' SECRET "$value" >"$tmp/env"
    expected=$(jq -cn --arg value "$value" '{SECRET: $value}')
    read_by_preflight=$(preflight_parse "$tmp/env")
    if [[ -z "$read_by_preflight" ]]; then
        printf '%s\n' "${refused[@]}" | grep -qxF -- "$value" || fail "preflight refused '$value'"
        continue
    fi
    printf '%s\n' "${accepted[@]}" | grep -qxF -- "$value" || fail "preflight accepted '$value'"
    [[ "$read_by_preflight" == "$expected" ]] || fail "preflight read '$value' as $read_by_preflight"
    [[ "$(cli_parse "$tmp/env")" == "$expected" ]] || fail "the CLI reads '$value' as $(cli_parse "$tmp/env")"
done
echo "env file round-trip test passed"
