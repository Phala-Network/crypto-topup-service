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

# Every image variable the compose references (TOPUP_IMAGE and POSTGRES_WALG_IMAGE for the
# service, PRODUCT_IMAGE for deploy/product/docker-compose.yml) must be a valid digest.
names=$(grep -o '[$][{][A-Z_]*_IMAGE:-' "$compose" | sed 's/^[$][{]//; s/:-$//' | sort -u)
if [ -z "$names" ]; then
    echo "$compose references no *_IMAGE variable" >&2
    exit 64
fi
for name in $names; do
    eval "value=\${$name:-}"
    validate_image "$name" "$value"
    export "$name=$value"
done

awk -v names="$names" '
BEGIN { count = split(names, name, /[[:space:]]+/) }
{
    line = $0
    for (i = 1; i <= count; i++) gsub("[$][{]" name[i] ":-[^}]*[}]", ENVIRON[name[i]], line)
    print line
}
' "$compose"
