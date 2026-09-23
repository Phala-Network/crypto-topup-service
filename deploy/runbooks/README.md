# Operations runbooks

These runbooks implement architecture sections 10-16 and plan work package D5. C12 (admin
operations), C8 (reconciliation), C10 (rate locks), C7b (flush pause), and A2 (deterministic
deployment) are on `main`. D3 backup and restore (#58) is covered by `deploy/RESTORE.md` and `deploy/local/restore-drill.sh`.

## Required environment

Use an application-role `DATABASE_URL`; every diagnostic transaction below also declares
`READ ONLY`. Load operator values from the attested route and secret manager, never from chat or a
ticket:

```sh
export BASE_URL=https://topup.example.internal
export ROUTE=phala-cloud-sepolia-pha-usd
export ROUTE_FILE=deploy/config/routes/phala-cloud-sepolia-pha.yaml
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

`BASE_URL` must be the service's `TOPUP_PUBLIC_ORIGIN`: the service verifies `@target-uri`
against that origin, so a signature over any other host or scheme fails with `401`.

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

## Alert and symptom index

Alert names are from `deploy/alerts/prometheus-rules.yml`.

| Alert or symptom | Runbook |
|---|---|
| `TopupReconciliationMismatch{check="address_derivation"}`, `423 chain_frozen` | [Chain frozen](chain-frozen.md) |
| `TopupReconciliationMismatch` (any other `check`), `TopupLoopStopped{loop="reconciler"}` | [Reconciliation mismatch](reconciliation-mismatch.md) |
| `TopupLockExposureNearCap`, `409 exposure_cap_exceeded` | [Lock exposure near cap](lock-exposure-near-cap.md) |
| `TopupLockExpiryFailing`, `TopupLockExposureDrift`, `TopupLoopStopped{loop="lock_expiry"}`, `rate-lock expiry scan failed` log, overdue open locks | [Lock expiry worker failure](lock-expiry-worker-failure.md) |
| `TopupScannerLag`, `TopupLoopStopped{loop="scanner"}` | [Scanner lag](scanner-lag.md) |
| `TopupBackupTooOld` | [Backup age](backup-age.md) |
| `TopupOperatorGasReserveLow` | [Gas refill](gas-refill.md) |
| `TopupDepositStateAgeExceeded{state="detected"}`, `topup_provider_disagreements_total` | [Provider disagreement](provider-disagreement.md), then [Price outage](price-outage.md) |
| `TopupDepositStateAgeExceeded{state="confirmed"}` (sanctions screen retrying) | [Provider disagreement](provider-disagreement.md) |
| `TopupDepositStateAgeExceeded{state="cleared"}`, repeated `processing`/`409` | [Stuck settlement](stuck-settlement.md) |
| Settlement HTTP `422`, `resend_forbidden=true` | [422 payload mismatch](payload-mismatch-422.md) |
| `TopupLoopStopped{loop="flusher"}`, reverted flush, isolated forwarder | [Flush reverted or bisected](flush-reverted-or-bisected.md) |
| `TopupLoopStopped{loop="outbox"}`, `topup_outbox_backlog` growth | [Outbox backlog](outbox-backlog.md) |
| `TopupLoopStopped{loop="pump"}` | [Stuck settlement](stuck-settlement.md) |
| `TopupUnsupportedInflows`, rejected funds reported at treasury | [Rejected funds at treasury](rejected-funds-at-treasury.md) |
| Unauthorized operator transaction, consumed nonce without receipt | [Operator key compromise](operator-key-compromise.md) |
| `OperatorRoleMissing` flusher alert, `flusher paused: the configured operator does not hold OPERATOR_ROLE` log | [Operator key compromise](operator-key-compromise.md): expected after an emergency revoke until the next key version is deployed; otherwise the new version was deployed before its grant |
| Database loss or restore drill | [Restore](restore.md) |
| Approved treasury migration | [Treasury change](treasury-change.md) |
| Approved refund ready for Safe execution | [Refund execution](refund-execution.md) |
| Any customer-impacting incident | [Incident communication](incident-communication.md) |
| User paid to their deposit address on another EVM chain | [Wrong-network deposit](wrong-network-deposit.md) |
| Removing a route version or a chain's last route | [Route or chain retirement](route-retirement.md) |

## Exercise status

Exercises ran against a task-scoped PostgreSQL 16 container and local Anvil, with real `topup` CLI
invocations or the repository's PostgreSQL/Anvil integration tests. A box is checked only when the
runbook's service-side procedure ran end to end with seeded, non-empty data. Human-only Safe,
Compliance, and publication steps are never exercised locally, and alert firing is not evidence
for any runbook.

| Runbook | Local status | G2 exercised once |
|---|---|---|
| Operator key compromise | Partial: key-version rotation, role gate, and revoke while running covered by the Anvil integration test; Finance Safe execution is human-only | [ ] |
| Provider disagreement | Partial: sanctions truth table; chain-evidence fixture missing | [ ] |
| Price outage | Partial; blocked on controllable price-source fixtures | [ ] |
| Stuck settlement | Partial: seeded nudge; blocked on a `processing`/`409` mock product | [ ] |
| 422 payload mismatch | Complete: 422, no resend, GET-first adoption with the mock product | [x] |
| Restore | Partial: local `deploy/local/restore-drill.sh` controlled and crash drills pass; staging drill pending | [ ] |
| Treasury change | Partial; remaining steps are human-only Safe/deployment work | [ ] |
| Gas refill | Partial; remaining transfer is human-only Safe work | [ ] |
| Refund execution | Complete: request, approve, record, finality-checked confirm | [x] |
| Rejected funds at treasury | Partial: seeded report; Compliance/Safe work remains | [ ] |
| Outbox backlog | Partial; blocked on a seeded delivered event and receiver | [ ] |
| Scanner lag | Partial; blocked on a controllable dual-provider chain fixture | [ ] |
| Flush reverted or bisected | Complete: Anvil selective revert, fresh nonce, bisect, isolation | [x] |
| Lock exposure near cap | Complete: seeded ledger query and C10 cap enforcement | [x] |
| Lock expiry worker failure | Partial: running service, injected ledger drift, owner repair while running; the in-service `topup reconcile` repair that replaced it is covered by the PostgreSQL integration tests only | [ ] |
| Reconciliation mismatch | Complete: `topup reconcile` findings, blocks, owner-only lift | [x] |
| Chain frozen | Complete: freeze, dual-provider check, owner lift, re-freeze, clean pass | [x] |
| Backup age | Partial: D3 archiving and local restore drills available | [ ] |
| Incident communication | Partial; publication and role actions are human-only | [ ] |
| Wrong-network deposit | Not exercised; every recovery step is human-only Finance and deployer work | [ ] |
| Route or chain retirement | Not exercised; the upgrade itself is human-only Safe work | [ ] |

Run `make runbook-check` after editing any runbook. Exercise evidence is under
[`exercises/`](exercises/).
