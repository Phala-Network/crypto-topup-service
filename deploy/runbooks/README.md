# Operations runbooks

Each runbook starts from a Sentry alert or Crons monitor ([deploy/README.md, "Sentry"](../README.md#sentry))
and works only through the surfaces a production CVM offers: it has no SSH, no logs, and no
database access. Run `make runbook-check` after editing a runbook: it checks every `topup` command
against the CLI and every API call against `crates/topup/openapi.json`.

## Surfaces

| Surface | What it shows or does |
|---|---|
| Sentry | the issue: its `alert` tag and grouping tags (`route`, `state`, `check`, `chain`, `scope`), a `runbook` link, and the log line's fields (for example `deposit_id`, or a finding's `subjects`, `expected`, `observed`); at most one event per issue every 10 minutes. Crons monitors for every loop; an Uptime monitor on `/healthz` |
| Daily report, admin-signed `GET /v1/admin/report/daily` | per route: `deposits_by_state`, `age_in_state_max_seconds`, `settlements_by_status`, `refunds_by_status`, `unflushed_balance_atomic`, `open_rate_lock_exposure_atomic`, `rejected_holds_atomic`, `treasury_balance_atomic`, and `flush_planning` (`at`, `outcome`, `error`); globally `exposure_minor`, the last reconciliation round's `failed_checks`, and the active `reconciliation_blocks` (`block_key`, `scope`, `check`, `reason`) |
| Support lookup, product-signed | `GET /v1/products/{p}/deposits?tx_hash=\|address=\|lock_ref=`: each deposit with its transition timeline (each step's evidence) and its webhook `events` (`id`, `event_type`, `delivered_at`); `GET /v1/products/{p}/deposits/{id}`: the deposit. Signed with the product's key, so run by the product's support tooling (staging: the reference product's seed) |
| Attestation, `GET /v1/attestation?nonce=` | the settlement key and each chain's flusher operator address ([verification](../README.md#attestation-ingress-and-egress)) |
| Chain | `cast` reads through both RPC providers: balances, nonces, roles, receipts, `addressOf` |
| Admin actions, admin-signed | route `pause`/`resume` of the scopes `quotes`, `addresses`, `settlement`, `flush`, `refunds`; deposit `nudge`; refund `approve`/`record`; product issue and key replacement; reconciliation block `lift`; outbox event `replay` |
| Phala Cloud, **HUMAN-ONLY** with the Environment's `PHALA_CLOUD_API_KEY` | `npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"` (or `stop`): the whole CVM, every container; state is in the database, so loops resume from it |

Database rows the API does not expose (reconciliation findings, flush rows, delivery attempts,
events not about a deposit, audit) and log lines other than the errors and alerts Sentry receives
are not observable in production. A restore-check instance ([RESTORE.md](../RESTORE.md)) serves
the same read API on a copy restored from backup and reports row counts and a full reconciliation
round's findings on its `/healthz`.

## Environment

Load values from the attested route and the admin's key store, never from chat or a ticket.
`BASE_URL` must be the service's `TOPUP_PUBLIC_ORIGIN`, or signatures fail with `401`.

```sh
export BASE_URL=https://crypto-topup-api.phala.com   # staging: https://crypto-topup-api-staging.phala.com
export ROUTE=phala-cloud-sepolia-pha-usd CHAIN_ID=11155111
export RPC_PROVIDER_A_URL=https://provider-a.example RPC_PROVIDER_B_URL=https://provider-b.example
export FACTORY=0x... IMPLEMENTATION=0x... TOKEN=0x... TREASURY=0x... OPERATOR_ADDRESS=0x...
export ADMIN_KEY_FILE=admin.pem ADMIN_KEY_ID=admin/v1
# admin METHOD PATH [JSON BODY]: signs the exact body (single-use, valid five minutes) and sends it.
admin() {
  printf '%s' "${3:-}" > /tmp/topup-admin-body
  mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh "$1" "$BASE_URL$2" \
    /tmp/topup-admin-body "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
  curl --fail-with-body -sS -X "$1" -H 'content-type: application/json' \
    -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
    --data-binary @/tmp/topup-admin-body "$BASE_URL$2"
}
admin GET /v1/admin/report/daily | jq
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["settlement"]}'
```

A pause answers `200` with the route's `paused_scopes`; `resume` takes the same body. A flush
already broadcast still confirms after a `flush` pause; an unsigned plan is voided and planned
again after resume. Route config (providers, caps, thresholds, operator key version) is attested:
changing it is a route PR and Deploy `upgrade` ([deploy/README.md, "Deploy"](../README.md#deploy)).

## Alert and symptom index

| Alert, monitor, or symptom | Runbook |
|---|---|
| `TopupReconciliationMismatch` (`check:address_derivation`), `423 chain_frozen` | [Chain frozen](chain-frozen.md) |
| `TopupReconciliationMismatch` (other `check`), `topup-reconciler` | [Reconciliation mismatch](reconciliation-mismatch.md) |
| `TopupDepositStateAgeExceeded` (`state:detected` or `state:confirmed`) | [Provider disagreement](provider-disagreement.md), then [Price outage](price-outage.md) |
| `TopupDepositStateAgeExceeded` (`state:credited`: not swept after 48 hours) | [Flush reverted or bisected](flush-reverted-or-bisected.md); a forwarder below the route's `min_flush_atomic` is never swept |
| `TopupLockExposureNearCap`, `409 exposure_cap_exceeded` | [Lock exposure near cap](lock-exposure-near-cap.md) |
| `TopupLockExpiryFailing`, `topup-lock-expiry` | [Lock expiry worker failure](lock-expiry-worker-failure.md) |
| `topup-scanner-<chain_id>` | [Scanner lag](scanner-lag.md) |
| `topup-backup` | [Backup age](backup-age.md) |
| `topup-outbox-<n>`, `outbox delivery poll failed`, daily report `credited_undelivered`, product reports missing webhooks or credits | [Outbox backlog](outbox-backlog.md) |
| `Reverted`, `IsolatedAddress`, `PlanningExcluded`, `FeeCapReached`, `topup-flush-<route>`, `flush_planning.outcome` `failed` or `send_failed` | [Flush reverted or bisected](flush-reverted-or-bisected.md) |
| `TopupOperatorGasReserveLow` | [Gas refill](gas-refill.md) |
| `OperatorRoleMissing`, `MissingConsumedReceipt`, unexplained operator transaction | [Operator key compromise](operator-key-compromise.md) |
| `TopupUnsupportedInflows`, rejected funds at the treasury | [Rejected funds at treasury](rejected-funds-at-treasury.md) |
| Product reports its request-signing key exposed, or product requests it did not make | [Product key compromise](product-key-compromise.md) |
| `NativeBalance` (native coin at a forwarder; the flusher never sweeps it) | No runbook: escalate to Finance and Engineering |
| Database loss, restore drill | [RESTORE.md](../RESTORE.md) |
| Approved refund | [Refund execution](refund-execution.md) |
| Approved treasury migration | [Treasury change](treasury-change.md) |
| Removing a route version or a chain's last route | [Route or chain retirement](route-retirement.md) |
| Payment sent on another EVM chain | [Wrong-network deposit](wrong-network-deposit.md) |
| Any customer-impacting incident | [Incident communication](incident-communication.md) |

## Exercise status

Local exercises ran against PostgreSQL and Anvil with the `topup` CLI or the integration tests;
Safe, Compliance, and publication steps are human-only and were never exercised. Except the staging
restore drill, every exercise ran the earlier, database-level form of these runbooks; none has run
in its current form against a CVM.

| Runbook | Last run | Outcome | Evidence |
|---|---|---|---|
| Chain frozen | 2026-09-22, local | complete: freeze, dual-provider check, owner lift, re-freeze | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Reconciliation mismatch | 2026-09-22, local | complete: findings, blocks, owner-only lift | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Flush reverted or bisected | 2026-09-23, local | complete: selective revert, fresh nonce, bisection, isolation, pause voiding | [#83](https://github.com/Phala-Network/crypto-topup-service/pull/83) |
| Refund execution | 2026-09-22, local | complete: request, approve, record, finality-checked confirm | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Lock exposure near cap | 2026-09-22, local | complete: cap enforcement; the alert itself not evaluated | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Operator key compromise | 2026-09-22, local | partial: revoke, role gate, key-version rotation; Safe steps human-only | [#74](https://github.com/Phala-Network/crypto-topup-service/pull/74) |
| Restore | 2026-09-25 23:09–23:26 UTC, staging | complete: drill instance on 8081, every live isolation check passed; RTO 17 min; `restore_check` `ok`, post-restore reconciliation complete; restored heartbeat newer than the start anchor; dstack verifier `UpToDate` for the original app id; the backup prefix gained only the live instance's own WAL (no `.history`, nothing removed). The first attempt (21:55 UTC, on 8080) was aborted when the drill instance took live traffic | [#126](https://github.com/Phala-Network/crypto-topup-service/pull/126) |
| Lock expiry worker failure | 2026-09-22, local | partial: exercised a counter since removed | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Provider disagreement | 2026-09-22, local | partial: sanctions truth table; no disagreeing-provider fixture | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Price outage | 2026-09-22, local | partial: route pause; no controllable price-source fixture | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Rejected funds at treasury | 2026-09-22, local | partial: report; Compliance and Safe steps human-only | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Gas refill | 2026-09-22, local | partial: balance and nonce reads; the transfer is human-only | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Treasury change | 2026-09-22, local | partial: tooling refuses without Safe expectations; the rest is human-only | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Outbox backlog | 2026-09-22, local | partial: no controllable webhook receiver | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Scanner lag | 2026-09-22, local | partial: no controllable finalized-chain fixture | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Backup age | 2026-09-22, local | partial: predates encrypted backups; local restore drills cover archiving | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Incident communication | 2026-09-22, local | partial: publication and roles human-only | [#59](https://github.com/Phala-Network/crypto-topup-service/pull/59) |
| Wrong-network deposit | — | not exercised; every step is human-only | — |
| Product key compromise | 2026-09-26, local | partial: the key replacement and its hard cut in the API test `admin_product_key_replacement`; the product side not exercised | this runbook's PR |
| Route or chain retirement | — | not exercised; the upgrade is human-only | — |
