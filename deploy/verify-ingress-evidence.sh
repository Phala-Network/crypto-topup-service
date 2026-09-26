#!/bin/sh
set -eu

# Verifies the certificate evidence that dstack-ingress publishes at https://DOMAIN/evidences/
# (deploy/README.md, "Custom domain"), the chain of the dstack-ingress README:
# 1. sha256sum.txt lists exactly acme-account.json and cert-DOMAIN.pem, and both match it;
# 2. quote.json, verified by the official dstack verifier (dstack-verifier.sh), is a valid TDX quote
#    of app APP_ID whose report_data is SHA-256(sha256sum.txt) zero-padded to 64 bytes;
# 3. the certificate DOMAIN serves on 443 is the leaf of cert-DOMAIN.pem.
# So the key TLS terminates with was obtained inside a CVM of APP_ID. The quote dates from the last
# issuance, so it may attest an earlier compose of the app. Prints the ACME account URI, which an
# optional CAA record pins.
if [ "$#" -ne 2 ]; then
    echo "usage: verify-ingress-evidence.sh DOMAIN APP_ID" >&2
    exit 64
fi

root=$(CDPATH='' cd -- "$(dirname "$0")/.." && pwd)
domain=$1
app_id=$(printf '%s' "${2#0x}" | tr 'A-F' 'a-f')
tmp=$(mktemp -d)

cleanup() {
    find "$tmp" -depth -delete
}
trap cleanup EXIT INT TERM

for file in quote.json sha256sum.txt acme-account.json "cert-$domain.pem"; do
    curl -fsS --max-time 30 --retry 3 -o "$tmp/$file" "https://$domain/evidences/$file"
done

printf '%s\n' acme-account.json "cert-$domain.pem" >"$tmp/expected-files"
awk '{ print $2 }' "$tmp/sha256sum.txt" | sort | cmp -s - "$tmp/expected-files" || {
    echo "sha256sum.txt does not list exactly acme-account.json and cert-$domain.pem" >&2
    exit 1
}
(cd "$tmp" && sha256sum --check --quiet sha256sum.txt) || {
    echo "the evidence files do not match sha256sum.txt" >&2
    exit 1
}

jq -e '{attestation: null, quote, event_log, vm_config}
    | select((.quote | type) == "string" and (.vm_config | type) == "string")' \
    "$tmp/quote.json" >"$tmp/request.json" || {
    echo "quote.json lacks the quote or the vm_config" >&2
    exit 1
}
"$root/deploy/dstack-verifier.sh" <"$tmp/request.json" >"$tmp/result.json"
manifest=$(sha256sum "$tmp/sha256sum.txt" | cut -c1-64)
jq -e --arg app_id "$app_id" --arg report_data "$manifest" '
    .details.app_info.app_id == $app_id and .details.report_data == $report_data + ("0" * 64)
' "$tmp/result.json" >/dev/null || {
    echo "the evidence quote is not app $app_id binding sha256sum.txt" >&2
    jq '.details | {tcb_status, app_id: .app_info.app_id, report_data}' "$tmp/result.json" >&2
    exit 1
}

served=$(openssl s_client -connect "$domain:443" -servername "$domain" </dev/null 2>/dev/null |
    openssl x509 -outform DER | sha256sum | cut -c1-64)
published=$(openssl x509 -in "$tmp/cert-$domain.pem" -outform DER | sha256sum | cut -c1-64)
[ "$served" = "$published" ] || {
    echo "$domain serves a certificate other than the one in its evidence" >&2
    exit 1
}

jq -r '"dstack verifier: quote and TCB \(.details.tcb_status), app id \(.details.app_info.app_id)",
    "evidence compose hash: 0x\(.details.app_info.compose_hash)"' "$tmp/result.json"
jq -r '"ACME account: \(.registration.accountURL // .registration.uri)"' "$tmp/acme-account.json"
echo "certificate evidence of $domain passed: the served certificate was obtained in app $app_id"
