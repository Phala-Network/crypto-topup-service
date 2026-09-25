#!/usr/bin/env bash
# Renders a compose for a CVM (deploy/README.md, "Attested settings"). Every `${NAME_IMAGE:-...}`
# becomes the repository@sha256 digest in NAME_IMAGE. Every other `${NAME:-}` whose NAME is not in
# ENV_EXAMPLE is a public setting: its value is taken from the environment and written inline, so
# it is attested with the compose hash. The names of ENV_EXAMPLE, the owner-sealed secrets, stay
# the only variables the output reads from the CVM's encrypted env. The service label
# `${..._RENDERED_SHA256:-}` becomes the digest of the rendered file: Compose recreates a
# container only when its service definition changes, so every rendered change recreates the
# services that carry it.
#
# The topup compose has two variants of the same source. The default is the service:
# TOPUP_RESTORE_FROM_BACKUP=off, TOPUP_SERVICE_ENABLED=on, and TOPUP_INGRESS_PORT=8080.
# --restore-check renders the restore verification instance (deploy/RESTORE.md):
# TOPUP_RESTORE_FROM_BACKUP=on (PostgreSQL restores and never archives, `backup` idles,
# `restore-check` runs), TOPUP_SERVICE_ENABLED=read-only, and TOPUP_INGRESS_PORT=8081, so the
# gateway never routes the service's port 8080 to it. The renderer sets all three; they are never
# read from the environment.
#
# --images-only renders the image digests alone (Release images uses it to validate digests).
# Values are never printed: errors name only the variable.
#
# Usage: deploy/render-compose.sh [--env-example FILE] [--restore-check | --images-only]
#          [SOURCE_COMPOSE] >docker-compose.rendered.yml
set -euo pipefail
export LC_ALL=C

root="$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)"
env_example="$root/deploy/staging.env.example"
variant=service
while (($#)); do
    case "$1" in
        --env-example) env_example="${2:?--env-example needs a file}"; shift 2 ;;
        --restore-check) variant=restore-check; shift ;;
        --images-only) variant=images-only; shift ;;
        -*) echo "usage: $0 [--env-example FILE] [--restore-check | --images-only] [SOURCE_COMPOSE]" >&2; exit 64 ;;
        *) break ;;
    esac
done
compose=${1:-"$root/deploy/docker-compose.yml"}

validate_image() {
    local name=$1 ref=$2 repository digest
    [[ "$ref" == *@sha256:* ]] || { echo "$name must be an image@sha256 reference" >&2; exit 64; }
    repository=${ref%@sha256:*}
    digest=${ref##*@sha256:}
    if [[ -z "$repository" || "$repository" == *[!A-Za-z0-9._/:+-]* ]]; then
        echo "$name has an invalid repository: $repository" >&2
        exit 64
    fi
    if [[ -z "$digest" || "$digest" == *[!0-9a-f]* ]]; then
        echo "$name has an invalid sha256 digest" >&2
        exit 64
    fi
    if ((${#digest} != 64)); then
        echo "$name sha256 digest must contain 64 lowercase hexadecimal characters" >&2
        exit 64
    fi
    if [[ "$digest" == 0000000000000000000000000000000000000000000000000000000000000000 ]]; then
        echo "$name must not use the zero digest placeholder" >&2
        exit 64
    fi
}

# Every image variable the compose references (TOPUP_IMAGE and POSTGRES_WALG_IMAGE for the
# service, PRODUCT_IMAGE for deploy/product/docker-compose.yml) must be a valid digest.
names=$(grep -o '[$][{][A-Z_]*_IMAGE:-' "$compose" | sed 's/^[$][{]//; s/:-$//' | sort -u)
if [[ -z "$names" ]]; then
    echo "$compose references no *_IMAGE variable" >&2
    exit 64
fi
for name in $names; do
    validate_image "$name" "${!name:-}"
    export "${name?}"
done

rest=$(awk -v names="$names" '
BEGIN { count = split(names, name, /[[:space:]]+/) }
{
    line = $0
    for (i = 1; i <= count; i++) gsub("[$][{]" name[i] ":-[^}]*[}]", ENVIRON[name[i]], line)
    print line
}
' "$compose")
if [[ "$variant" == images-only ]]; then
    printf '%s\n' "$rest"
    exit 0
fi

sealed=" $(awk '/^[[:space:]]*($|#)/ { next } { sub(/=.*/, ""); printf "%s ", $0 }' "$env_example")"
if [[ "$variant" == restore-check ]]; then
    TOPUP_RESTORE_FROM_BACKUP=on TOPUP_SERVICE_ENABLED=read-only TOPUP_INGRESS_PORT=8081
else
    TOPUP_RESTORE_FROM_BACKUP=off TOPUP_SERVICE_ENABLED=on TOPUP_INGRESS_PORT=8080
fi

# Values land in double-quoted YAML strings or JSON strings that Compose interpolates: allow
# printable ASCII without spaces, quotes, backslashes, or `$`.
for name in $(grep -o '[$][{][A-Z_][A-Z0-9_]*:-[}]' <<<"$rest" | sed 's/^[$][{]//; s/:-[}]$//' | sort -u); do
    [[ "$sealed" == *" $name "* || "$name" == *_RENDERED_SHA256 ]] && continue
    value=${!name-}
    if ! [[ "$value" =~ ^[[:graph:]]{1,512}$ ]] || [[ "$value" == *[\"\\\$]* ]]; then
        echo "render-compose.sh: $name must be 1-512 printable ASCII characters without spaces," \
            "quotes, backslashes, or \$" >&2
        exit 64
    fi
done

# Single pass: only the settings' `${NAME:-}` placeholders are replaced; values are not rescanned.
rendered=""
while [[ "$rest" =~ \$\{([A-Z_][A-Z0-9_]*):-\} ]]; do
    placeholder=${BASH_REMATCH[0]}
    name=${BASH_REMATCH[1]}
    rendered+=${rest%%"$placeholder"*}
    if [[ "$sealed" == *" $name "* || "$name" == *_RENDERED_SHA256 ]]; then
        rendered+=$placeholder
    else
        rendered+=${!name}
    fi
    rest=${rest#*"$placeholder"}
done
rendered+=$rest

label=$(grep -o '[$][{][A-Z_]*_RENDERED_SHA256:-[}]' <<<"$rendered" || true)
[[ -n "$label" && "$label" != *$'\n'* ]] || {
    echo "render-compose.sh: $compose must carry exactly one \${..._RENDERED_SHA256:-} label" >&2
    exit 64
}
# The owner runs the offline preflight, and so this renderer, on their own machine (macOS: shasum).
if command -v sha256sum >/dev/null; then sha256=(sha256sum); else sha256=(shasum -a 256); fi
digest=$(printf '%s\n' "$rendered" | "${sha256[@]}" | awk '{print $1}')
rendered=${rendered/"$label"/$digest}
left=$(grep -o '[$][{][^}]*[}]' <<<"$rendered" | sort -u || true)
expected=$(for name in $sealed; do printf '${%s:-}\n' "$name"; done | sort -u)
[[ "$left" == "$expected" ]] || {
    echo "render-compose.sh: the rendered compose must read exactly the names of $env_example" \
        "from the env" >&2
    exit 1
}
printf '%s\n' "$rendered"
