#!/usr/bin/env bash
# Phala Cloud CVM steps of .github/workflows/deploy.yml, with the locked CLI (deploy/phala: Phala
# Cloud CLI 1.1.22; PHALA_CLOUD_API_KEY and PHALA_CLOUD_DIR come from the environment).
#
# Usage:
#   deploy/phala-cvm.sh get CVM_ID >cvm.json
#   deploy/phala-cvm.sh url CVM_JSON PORT            the gateway URL of PORT
#   deploy/phala-cvm.sh gateway-domain CVM_JSON      the CVM's gateway host, gateway.BASE_DOMAIN: a
#                                                    custom domain's CNAME target
#   deploy/phala-cvm.sh deploy OUTPUT CLI_ARGS...    `deploy --json`; its output goes to the log, its
#                                                    JSON object to OUTPUT, and it must succeed
#   deploy/phala-cvm.sh wait CVM_ID [PREVIOUS_HASH] >cvm.json
#                                                    until the CVM runs, settled, with a compose hash
#                                                    other than PREVIOUS_HASH (15 minutes)
#   deploy/phala-cvm.sh attestation CVM_ID HASH >attestation.json
#                                                    until the attestation reports compose hash HASH
#                                                    (10 minutes; an upgrade attests late)
#   deploy/phala-cvm.sh healthz URL                  until URL/healthz answers (10 minutes)
set -euo pipefail

phala() {
    "$(dirname -- "$0")/phala" "$@"
}

# The compose hash of a `cvms get` or attestation document: lowercase hex without 0x.
normal_hash='ascii_downcase | ltrimstr("0x")'

command=${1:-}
shift || true
case "$command" in
    get)
        phala cvms get "$1" --json
        ;;
    url)
        jq -er --arg port "$2" '"https://\(.app_id | ltrimstr("0x"))-\($port).\(.gateway.base_domain)"' "$1"
        ;;
    gateway-domain)
        jq -er '"gateway.\(.gateway.base_domain)"' "$1"
        ;;
    deploy)
        output=$1
        shift
        # `deploy --json` writes `Provisioning CVM ...` before the JSON object on a new CVM.
        phala deploy --json "$@" 2>&1 | tee "$output.raw" >&2
        sed -n '/^{/,$p' "$output.raw" >"$output"
        jq -e '.success == true' "$output" >/dev/null
        ;;
    wait)
        settled='.status == "running" and (.in_progress | not)
            and ((.compose_hash | '"$normal_hash"') != $previous)'
        for _ in $(seq 60); do
            if cvm=$(phala cvms get "$1" --json) &&
                jq -e --arg previous "${2:-}" "$settled" <<<"$cvm" >/dev/null; then
                printf '%s\n' "$cvm"
                exit 0
            fi
            sleep 15
        done
        echo "::error::CVM $1 did not run a new compose within 15 minutes" >&2
        exit 1
        ;;
    attestation)
        attested=""
        for _ in $(seq 40); do
            if attestation=$(phala cvms attestation "$1" --json) &&
                attested=$(jq -r '[.tcb_info.event_log[]? | select(.event == "compose-hash")
                    | .event_payload][0] // "" | '"$normal_hash" <<<"$attestation") &&
                [[ "$attested" == "$2" ]]; then
                printf '%s\n' "$attestation"
                exit 0
            fi
            sleep 15
        done
        echo "::error::the attestation reports compose hash '${attested:-none}', not the deployed $2" >&2
        exit 1
        ;;
    healthz)
        for _ in $(seq 60); do
            curl -fsS --max-time 10 "$1/healthz" >/dev/null && exit 0
            sleep 10
        done
        echo "::error::$1/healthz did not answer" >&2
        exit 1
        ;;
    *)
        echo "usage: $0 get|url|gateway|deploy|wait|attestation|healthz ARGS..." >&2
        exit 64
        ;;
esac
