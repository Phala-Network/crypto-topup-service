#!/usr/bin/env bash
set -euo pipefail

usage() {
    echo "usage: $0 METHOD URL BODY_FILE ED25519_PRIVATE_KEY_PEM KEY_ID" >&2
    exit 2
}

[[ $# -eq 5 ]] || usage

method=${1^^}
url=$2
body_file=$3
key_file=$4
key_id=$5

[[ "$method" =~ ^[A-Z]+$ ]] || usage
[[ -f "$body_file" && -f "$key_file" ]] || usage
[[ "$url" != *$'\n'* && "$url" != *'"'* ]] || usage
[[ "$key_id" =~ ^[A-Za-z0-9._/-]+$ ]] || usage

created=$(date +%s)
digest=$(openssl dgst -sha256 -binary "$body_file" | openssl base64 -A)
content_digest="sha-256=:$digest:"
parameters="(\"@method\" \"@target-uri\" \"content-digest\");created=$created;keyid=\"$key_id\";alg=\"ed25519\""
tmp=$(mktemp -d)
trap 'find "$tmp" -depth -delete' EXIT INT TERM
printf '"@method": %s\n"@target-uri": %s\n"content-digest": %s\n"@signature-params": %s' \
    "$method" "$url" "$content_digest" "$parameters" >"$tmp/signature-base"
signature=$(openssl pkeyutl -sign -rawin -inkey "$key_file" -in "$tmp/signature-base" \
    | openssl base64 -A)

printf '%s\n' \
    "Content-Digest: $content_digest" \
    "Signature-Input: sig1=$parameters" \
    "Signature: sig1=:$signature:"
