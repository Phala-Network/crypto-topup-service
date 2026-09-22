# Operations runbooks

These runbooks implement architecture sections 14-16 and plan work package D5. They are written
against commit `6b868ea` on `main`. PRs #56 (alerts), #57 (C12 admin operations), and #58 (D3
restore) were not on `main` when these commands were exercised on 2026-09-22.

## Required environment

Use an application-role `DATABASE_URL`; every diagnostic transaction below also declares
`READ ONLY`. Load operator values from the attested route and secret manager, never from chat or a
ticket:

```sh
export BASE_URL=https://topup.example.internal
export ROUTE=phala-cloud-sepolia-pha-usd
export CHAIN_ID=11155111
export RPC_PROVIDER_A_URL=https://provider-a.example
export RPC_PROVIDER_B_URL=https://provider-b.example
export FACTORY=0x...
export IMPLEMENTATION=0x...
export TREASURY=0x...
export TOKEN=0x...
export OPERATOR_ADDRESS=0x...
export ADMIN_KEY_FILE=/run/secrets/topup-admin-ed25519.pem
export ADMIN_KEY_ID=admin/v1
export DATABASE_URL=postgres://topup_service:...@postgres/topup
```

For an authenticated admin request, write the exact body to a file, sign those exact bytes, then
pass the three returned headers to `curl`. Signatures are single-use and expire after five minutes:

```sh
printf '%s' '{"scopes":["settlement"]}' > /tmp/topup-admin-body.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST \
  "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/topup-admin-body.json \
  "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' \
  -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
  --data-binary @/tmp/topup-admin-body.json \
  "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

## Trigger map

PR #56 is not on `main`, so the canonical alert names do not yet exist under `deploy/alerts`.
Until it merges, route the metric or symptom below to the named runbook.

| Metric or symptom | Runbook |
|---|---|
| Unauthorized operator transaction, consumed nonce without receipt | [Operator key compromise](operator-key-compromise.md) |
| Two finalized providers disagree | [Provider disagreement](provider-disagreement.md) |
| Stale/unavailable/divergent price sources | [Price outage](price-outage.md) |
| Settlement age or repeated `processing`/`409` | [Stuck settlement](stuck-settlement.md) |
| Settlement HTTP `422`, `resend_forbidden=true` | [422 payload mismatch](payload-mismatch-422.md) |
| Database loss or restore drill | [Restore](restore.md) |
| Approved treasury migration | [Treasury change](treasury-change.md) |
| Operator gas reserve below policy | [Gas refill](gas-refill.md) |
| Approved refund ready for Safe execution | [Refund execution](refund-execution.md) |
| Rejected funds reported at treasury | [Rejected funds at treasury](rejected-funds-at-treasury.md) |
| Old undelivered outbox rows | [Outbox backlog](outbox-backlog.md) |
| Finalized head minus cursor exceeds policy | [Scanner lag](scanner-lag.md) |
| Reverted flush or isolated forwarder | [Flush reverted or bisected](flush-reverted-or-bisected.md) |
| Open lock exposure approaches a configured cap | [Lock exposure near cap](lock-exposure-near-cap.md) |
| Last successful backup older than 120 seconds | [Backup age](backup-age.md) |
| Any customer-impacting incident | [Incident communication](incident-communication.md) |

## Known command gaps on main

- `topup restore-check` is present in `topup --help` but exits with `restore-check is not
  implemented`.
- Admin nudge, refund approve/record, and daily report are documented in OpenAPI but return HTTP
  `501` owned by C12.
- There is no CLI command to derive or select `operator/v2`; Safe role rotation cannot be completed
  until the service can start with the new operator derivation.
- There is no deterministic factory deployment command on `main`; treasury migration stops before
  deployment rather than substituting an ad hoc deployment.
- Alert rules and stable metric names are pending PR #56. D3 encrypted backup/restore automation is
  pending PR #58. C10 lock reservation operations are not on `main`.

Run `make runbook-check` after editing any runbook. Exercise evidence is under
[`exercises/`](exercises/).
