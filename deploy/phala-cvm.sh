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
#   deploy/phala-cvm.sh wait [--unsealed] CVM_ID [PREVIOUS_HASH] >cvm.json
#                                                    until the CVM runs, settled, with a compose hash
#                                                    other than PREVIOUS_HASH (15 minutes); with
#                                                    --unsealed, or settled and failed as unsealed:
#                                                    booted (an instance id) and unsealed-boot
#   deploy/phala-cvm.sh unsealed-boot <serial.log    whether the serial console's latest boot failed
#                                                    only as an unsealed topup CVM does: 0 yes, 1 any
#                                                    other failure (printed), 2 not yet known
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

# A provisioned CVM is unsealed until its owner seals the secrets. Without the storage credentials
# topup's PostgreSQL refuses to start (deploy/scripts/postgres-walg-entrypoint.sh), so app-compose
# fails with exactly `dependency failed to start: container dstack-postgres-1 is unhealthy`;
# dstack reports boot.error with the VM and its guest agent up, and Phala Cloud shows the CVM as
# error. The refusal itself is in the container's log, which is not public; the console holds
# app-compose's output (dstack basefiles/app-compose.service). The latest boot is the console from
# the last banner of the pre-launch script, which app-compose runs before `docker compose up`. Any
# other failure line there (a pull, another container, the pre-launch script) is not this one.
unsealed_boot() {
    local boot unexpected
    boot=$(sed 's/\x1b\[[0-9;?]*[A-Za-z]//g; s/\r//g' | awk '
        /Running Phala Cloud Pre-Launch Script/ { count = 0; found = 1 }
        found { line[++count] = $0 }
        END { for (i = 1; i <= count; i++) print line[i] }')
    [[ -n "$boot" ]] || { echo "the serial console shows no boot of the pre-launch script" >&2; return 2; }
    unexpected=$(grep -iE 'error|fail|denied|unhealthy|exited|cannot|unable|not found|invalid|panic|timed out|timeout' \
        <<<"$boot" | grep -vE 'dependency failed to start: container dstack-postgres-1 is unhealthy$| Container dstack-postgres-1 +Error( [0-9.]+s)?$|app-compose\.service|App Compose Service' ||
        true)
    if [[ -n "$unexpected" ]]; then
        echo "the CVM failed otherwise than unsealed; its console's latest boot shows:" >&2
        printf '%s\n' "$unexpected" >&2
        return 1
    fi
    if ! grep -q 'Starting containers' <<<"$boot" ||
        ! grep -qE 'dependency failed to start: container dstack-postgres-1 is unhealthy$' <<<"$boot"; then
        echo "the console's latest boot does not show app-compose's unsealed failure yet" >&2
        return 2
    fi
}

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
        # Settled: no operation in progress, with a new compose. An unsealed CVM settles without
        # running when its latest boot failed only as unsealed (unsealed_boot).
        unsealed=false outcome="run a new compose"
        [[ "${1:-}" != --unsealed ]] || { unsealed=true outcome="run or fail as unsealed"; shift; }
        settled='(.in_progress | not) and ((.compose_hash | '"$normal_hash"') != $previous)'
        for _ in $(seq 60); do
            if cvm=$(phala cvms get "$1" --json); then
                jq -r '"status=\(.status) in_progress=\(.in_progress) compose_hash=\(.compose_hash)"' \
                    <<<"$cvm" >&2 || true
                if jq -e --arg previous "${2:-}" "$settled" <<<"$cvm" >/dev/null; then
                    if jq -e '.status == "running"' <<<"$cvm" >/dev/null; then
                        printf '%s\n' "$cvm"
                        exit 0
                    fi
                    if [[ "$unsealed" == true ]] && jq -e '(.instance_id // "") != ""' <<<"$cvm" >/dev/null &&
                        serial=$(phala logs --serial --cvm-id "$1"); then
                        verdict=0
                        unsealed_boot <<<"$serial" || verdict=$?
                        case "$verdict" in
                            0)
                                echo "the CVM failed as unsealed, as expected: it starts once sealed" >&2
                                printf '%s\n' "$cvm"
                                exit 0
                                ;;
                            1)
                                echo "::error::CVM $1 did not boot as an unsealed CVM does (above)" >&2
                                exit 1
                                ;;
                        esac
                    fi
                fi
            fi
            sleep 15
        done
        echo "::error::CVM $1 did not $outcome within 15 minutes" >&2
        exit 1
        ;;
    unsealed-boot)
        unsealed_boot
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
        echo "usage: $0 get|url|gateway-domain|deploy|wait|unsealed-boot|attestation|healthz ARGS..." >&2
        exit 64
        ;;
esac
