#!/usr/bin/env bash
# Renders the attested compose of one CVM (docs/design/deploy-config.md, "Rendering") with the
# pinned Docker Compose the CVM runs (pinned-compose.sh):
#
#   1. Merges the stack with the environment directory ENV_DIR (`compose.yaml` and `topup.yaml` for
#      a topup CVM, `compose.yaml` and `config.json` for the reference product,
#      deploy/product/compose.yaml) and, for a topup CVM, its variant: compose.service.yaml, or
#      compose.restore-check.yaml with --restore-check.
#      `config --no-interpolate` keeps the sealed secrets as `${NAME:-}` references.
#   2. Applies the three deploy-time inputs and nothing else: --images pins each image the stack
#      names by image name, --gateway-domain is dstack-ingress's gateway (the service variant and
#      the product), and --origin is the restore instance's own origin (restore-check only). Every
#      config file is inlined as content named after its digest, so a changed file changes the
#      definition of exactly the services that mount it.
#   3. Prints Compose's canonical YAML after deploy/compose-policy.jq accepted it.
#
# The project is `dstack`, the name dstack gives the stack it runs in /dstack, so the volumes keep
# their names; --project-name is for local rehearsals only. Values are never printed on error.
#
# Usage: deploy/render.sh [--restore-check] --images FILE [--gateway-domain HOST | --origin URL]
#          [--project-name NAME] [--no-download] ENV_DIR >docker-compose.yml
set -euo pipefail
export LC_ALL=C

root="$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)"
usage() {
    echo "usage: $0 [--restore-check] --images FILE [--gateway-domain HOST | --origin URL]" \
        "[--project-name NAME] [--no-download] ENV_DIR" >&2
    exit 64
}
restore_check=0 images="" gateway="" origin="" project=dstack download=()
while (($#)); do
    case "$1" in
        --restore-check) restore_check=1; shift ;;
        --images) images=${2:-}; shift 2 ;;
        --gateway-domain) gateway=${2:-}; shift 2 ;;
        --origin) origin=${2:-}; shift 2 ;;
        --project-name) project=${2:-}; shift 2 ;;
        --no-download) download=(--no-download); shift ;;
        -*) usage ;;
        *) break ;;
    esac
done
(($# == 1)) || usage
env_dir=$(CDPATH='' cd -- "$1" && pwd) || { echo "$1 is not a directory" >&2; exit 64; }
[[ -f "$env_dir/compose.yaml" ]] || { echo "$env_dir has no compose.yaml" >&2; exit 64; }
[[ -f "$images" ]] || { echo "--images must name the release's images.json" >&2; exit 64; }
[[ "$project" =~ ^[a-z0-9][a-z0-9_-]*$ ]] || { echo "invalid --project-name" >&2; exit 64; }

if [[ -f "$env_dir/topup.yaml" && ! -f "$env_dir/config.json" ]]; then
    stack=(-f "$root/deploy/compose.yaml") config_name=topup config_file="$env_dir/topup.yaml"
    variant=service
elif [[ -f "$env_dir/config.json" && ! -f "$env_dir/topup.yaml" ]]; then
    stack=(-f "$root/deploy/product/compose.yaml") config_name=product
    config_file="$env_dir/config.json" variant=product
    ((!restore_check)) || { echo "--restore-check renders a topup environment only" >&2; exit 64; }
else
    echo "$env_dir must hold topup.yaml (a topup CVM) or config.json (the product)" >&2
    exit 64
fi
if ((restore_check)); then
    variant=restore-check
    stack+=(-f "$env_dir/compose.yaml" -f "$root/deploy/compose.restore-check.yaml")
    [[ -z "$gateway" && "$origin" =~ ^https://[a-z0-9]([a-z0-9.-]*[a-z0-9])?(:[0-9]{1,5})?$ ]] || {
        echo "--restore-check needs --origin https://HOST (and no --gateway-domain)" >&2
        exit 64
    }
else
    stack+=(-f "$env_dir/compose.yaml")
    [[ "$variant" == product ]] || stack+=(-f "$root/deploy/compose.service.yaml")
    [[ -z "$origin" && "$gateway" =~ ^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)+$ ]] || {
        echo "the $variant variant needs --gateway-domain HOST (and no --origin)" >&2
        exit 64
    }
fi
jq -e 'type == "object" and all(to_entries[];
        (.key | test("^[a-z0-9][a-z0-9._-]*$"))
        and (.value | test("^[a-z0-9]+([._/:-][a-z0-9]+)*@sha256:[0-9a-f]{64}$"))
        and (.value | test("@sha256:0{64}$") | not))' "$images" >/dev/null || {
    echo "--images must map image names to nonzero repository@sha256:<64 hex> references" >&2
    exit 64
}

compose=$("$root/deploy/pinned-compose.sh" "${download[@]}")
tmp=$(mktemp -d "${TMPDIR:-/tmp}/topup-render.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

# The environment's own configuration file stands in for the stack's placeholder.
jq -n --arg name "$config_name" --arg file "$config_file" '{configs: {($name): {file: $file}}}' \
    >"$tmp/config.json"
"$compose" -p "$project" --project-directory "$root/deploy" "${stack[@]}" -f "$tmp/config.json" \
    config --no-interpolate --format json >"$tmp/merged.json"

# Every file config, read here: its content with `$` escaped for Compose, and its digest.
: >"$tmp/contents.jsonl"
while IFS=$'\t' read -r name file; do
    [[ -f "$file" ]] || { echo "config $name: $file does not exist" >&2; exit 1; }
    digest=$(if command -v sha256sum >/dev/null; then sha256sum "$file"; else shasum -a 256 "$file"; fi |
        cut -c1-12)
    jq -n --arg name "$name" --arg digest "$digest" --rawfile content "$file" \
        '{name: $name, renamed: "\($name)_\($digest)", content: ($content | gsub("\\$"; "$$"))}' \
        >>"$tmp/contents.jsonl"
done < <(jq -r '.configs // {} | to_entries[] | select(.value.file) | [.key, .value.file] | @tsv' \
    "$tmp/merged.json")

jq --slurpfile images "$images" --slurpfile configs <(jq -s . "$tmp/contents.jsonl") \
    --arg variant "$variant" --arg gateway "$gateway" --arg origin "$origin" '
    ($configs[0] | map({key: .name, value: .}) | from_entries) as $files
    | def environment_map: if type == "array"
        then map(capture("^(?<key>[^=]+)=(?<value>.*)$")) | from_entries else . end;
    .services |= with_entries(.value |= (
        (if has("image") and ($images[0][.image] != null) then .image = $images[0][.image] else . end)
        | (if has("environment") then .environment |= environment_map else . end)
        | (if has("configs") then .configs |= map(.source |= ($files[.].renamed // .)) else . end)))
    | .configs |= with_entries(if $files[.key] then
        {key: $files[.key].renamed, value: {content: $files[.key].content}} else . end)
    | if $variant == "restore-check" then
        .services.topup.command += ["--public-origin", $origin]
      else
        .services["dstack-ingress"].environment.GATEWAY_DOMAIN = $gateway
      end
' "$tmp/merged.json" >"$tmp/pinned.json"

"$compose" -p "$project" -f "$tmp/pinned.json" config --no-interpolate >"$tmp/rendered.yml"
"$compose" -p "$project" -f "$tmp/rendered.yml" config --no-interpolate --format json \
    >"$tmp/rendered.json"
violations=$(jq -r -L "$root/deploy" --arg variant "$variant" --arg project "$project" \
    'include "compose-policy"; violations($variant; $project)[]' "$tmp/rendered.json")
if [[ -n "$violations" ]]; then
    echo "render.sh: the rendered $variant compose breaks deploy/compose-policy.jq:" >&2
    printf '  %s\n' "$violations" >&2
    exit 1
fi
cat "$tmp/rendered.yml"
