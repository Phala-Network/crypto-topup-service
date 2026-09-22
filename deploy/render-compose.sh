#!/bin/sh
set -eu

root=$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)
compose=${1:-"$root/deploy/docker-compose.yml"}

validate_image() {
    name=$1
    ref=$2
    case "$ref" in
        *@sha256:*) ;;
        *) echo "$name must be an image@sha256 reference" >&2; exit 64 ;;
    esac

    repository=${ref%@sha256:*}
    digest=${ref##*@sha256:}
    case "$repository" in
        ''|*[!A-Za-z0-9._/:+-]*)
            echo "$name has an invalid repository: $repository" >&2
            exit 64
            ;;
    esac
    case "$digest" in
        *[!0-9a-f]*|'') echo "$name has an invalid sha256 digest" >&2; exit 64 ;;
    esac
    if [ "${#digest}" -ne 64 ]; then
        echo "$name sha256 digest must contain 64 lowercase hexadecimal characters" >&2
        exit 64
    fi
    if [ "$digest" = "0000000000000000000000000000000000000000000000000000000000000000" ]; then
        echo "$name must not use the zero digest placeholder" >&2
        exit 64
    fi
}

validate_image TOPUP_IMAGE "${TOPUP_IMAGE:-}"
validate_image POSTGRES_WALG_IMAGE "${POSTGRES_WALG_IMAGE:-}"

awk -v topup="$TOPUP_IMAGE" -v postgres="$POSTGRES_WALG_IMAGE" '
{
    line = $0
    gsub(/[$][{]TOPUP_IMAGE:-[^}]*[}]/, topup, line)
    gsub(/[$][{]POSTGRES_WALG_IMAGE:-[^}]*[}]/, postgres, line)
    print line
}
' "$compose"
