# Operations runbooks

Each runbook starts from a Sentry alert or Crons monitor ([deploy/README.md, "Sentry"](../README.md#sentry))
and works only through the surfaces a production CVM offers: it has no SSH, no logs, and no
database access. Run `make runbook-check` after editing a runbook: it checks every `topup` command
against the CLI and every API path against `crates/topup/openapi.json` and
`crates/topup/openapi.admin.json`.

## Surfaces

| Surface | What it shows or does |
|---|---|
| Sentry | the issue: its `alert` tag and grouping tags (`route`, `state`, `check`, `chain_id`, `scope`, `id`), a `runbook` link, and the log line's fields (for example `deposit_id`, or a finding's `subjects`, `expected`, `observed`); at most one event per issue every 10 minutes. Crons monitors for every loop; an Uptime monitor on `/healthz` |
| Daily report, admin-signed `GET /v1/admin/reports/daily` | per route: `deposits_by_state`, `age_in_state_max_seconds`, `refunds_by_status`, `credited_undelivered` and `credited_undelivered_max_age_seconds` (credited deposits whose `deposit.credited` is not delivered yet), `unflushed_balance_atomic` (what forwarders still hold for merchants to sweep), `open_rate_lock_exposure_atomic`, and `rejected_holds_atomic`; globally `exposure_minor`, the last reconciliation round's `failed_checks`, the active `reconciliation_blocks` (`block_key`, `scope`, `check`, `reason`), and `failing_webhook_endpoints`: every enabled endpoint of any account whose oldest undelivered event is older than `failing_for_hours` (default 24; `?failing_for_hours=` 1 to 720), with its `account`, `livemode`, `url`, `pending_deliveries`, `oldest_pending_at`, and `last_attempt_status` ([outbox backlog](outbox-backlog.md)) |
| Deposit view, admin-signed `GET /v1/admin/deposits/{id}` (`dep_…`) | the deposit as its account sees it, with `admin`: the processing `state`, route, transition timeline (each step's evidence), and webhook `events` (`id`, `type`, `delivered_at`). The merchant finds deposits with its own `GET /v1/deposits?tx_hash=…` or `?client_reference_id=…` |
| Attestation, `GET /v1/attestation?nonce=`, with an account's API key | that account's webhook keys in the key's mode ([verification](../README.md#attestation-ingress-and-egress)) |
| Chain | `cast` reads through both RPC providers: balances, nonces, receipts, `addressOf`, the factory's `Flushed` and `FlushFailed` logs |
| Admin actions, admin-signed | route and account `pause`/`resume` of the scopes `quotes`, `settlement`, `refunds`; a treasury's crediting `pause`/`resume`; a customer's `quotes` pause; deposit `nudge` (`dep_…`); account creation and update (`charges_enabled` for live access, `restricted`, `max_unfinalized_credit`, `contact`) and recovery keys ([deploy/README.md, "Account credentials"](../README.md#account-credentials)); reconciliation block `lift`; after a restore, the freeze's status and the reconciliation under `/v1/admin/restore` ([Reconciliation after a restore](restore.md)). Merchants manage their webhook endpoints and resend their events themselves |
| Phala Cloud, **HUMAN-ONLY** with the Environment's `PHALA_CLOUD_API_KEY` | `deploy/phala cvms restart "$TOPUP_CVM_ID"` (or `stop`), in the kit's directory (`deploy/phala` runs its locked CLI; `npm ci --prefix deploy/tools --ignore-scripts` once): the whole CVM, every container; state is in the database, so loops resume from it |

Database rows the API does not expose (reconciliation findings, `flushed` and `flush_failures`,
delivery attempts,
events not about a deposit, audit) and log lines other than the errors and alerts Sentry receives
are not observable in production. A restore-check instance ([RESTORE.md](../RESTORE.md)) serves
the same read API on a copy restored from backup and reports row counts and a full reconciliation
round's findings on its `/healthz`.

## Environment

Load values from the attested configuration and the admin's key store, never from chat or a
ticket. `BASE_URL` must be the service's `public_origin` (the Environment's `topup.yaml`; Phala's
instance: `https://pay-api.phala.com`, staging `https://pay-api-staging.phala.com`), or signatures
fail with `401`; `ADMIN_KEY_ID` is its `admin_key.id`, `admin/<Environment>-v1` unless rotated
([deploy/README.md, "Attested settings"](../README.md#attested-settings)).

```sh
export BASE_URL="https://pay-api-staging.phala.com"   # the Environment's public_origin
# The affected route; staging also serves phala-cloud-sepolia-{usdc,usdt}-usd, and on Base Sepolia
# (CHAIN_ID=84532, providers base-sepolia-a/-b) phala-cloud-base-sepolia-{pha,usdc,usdt}-usd.
export ROUTE=phala-cloud-sepolia-pha-usd CHAIN_ID=11155111
export RPC_PROVIDER_A_URL=https://provider-a.example RPC_PROVIDER_B_URL=https://provider-b.example
export FACTORY=0x... IMPLEMENTATION=0x... TOKEN=0x... TREASURY=0x...
export ADMIN_KEY_FILE=admin.pem ADMIN_KEY_ID=admin/production-v1
# admin METHOD PATH [JSON BODY]: signs the exact body (single-use, valid five minutes) and sends it.
admin() {
  printf '%s' "${3:-}" > /tmp/topup-admin-body
  mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh "$1" "$BASE_URL$2" \
    /tmp/topup-admin-body "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
  curl --fail-with-body -sS -X "$1" -H 'content-type: application/json' \
    -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
    --data-binary @/tmp/topup-admin-body "$BASE_URL$2"
}
admin GET /v1/admin/reports/daily | jq
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["settlement"]}'
```

A pause answers `200` with the route's `paused_scopes`; `resume` takes the same body. The service
sends no transactions, so no pause stops a sweep: anyone can call the factory's `flush`, and a
forwarder only ever pays its own treasury. Route config (providers, caps, thresholds) is attested:
changing it is a route PR and Deploy `upgrade` ([deploy/README.md, "Deploy"](../README.md#deploy)).

## Alert and symptom index

| Alert, monitor, or symptom | Runbook |
|---|---|
| `TopupReconciliationMismatch` (`check:address_derivation` or `check:custody_balance`), `400 chain_frozen` | [Chain frozen](chain-frozen.md) |
| `TopupReconciliationMismatch` (other `check`), `topup-reconciler` | [Reconciliation mismatch](reconciliation-mismatch.md) |
| `TopupDepositStateAgeExceeded` (`state:detected` or `state:confirmed`) | [Provider disagreement](provider-disagreement.md), then [Price outage](price-outage.md) |
| `TopupLockExposureNearCap`, `400 exposure_cap_exceeded` | [Lock exposure near cap](lock-exposure-near-cap.md) |
| `TopupLockExpiryFailing`, `topup-lock-expiry` | [Lock expiry worker failure](lock-expiry-worker-failure.md) |
| `topup-scanner-<chain_id>` | [Scanner lag](scanner-lag.md) |
| `topup-backup` | [Backup age](backup-age.md) |
| `topup-outbox-test`, `topup-outbox-live`, `outbox delivery claim failed` or `outbox delivery failed`, daily report `credited_undelivered` or `failing_webhook_endpoints`, a merchant reports missing webhooks or credits | [Outbox backlog](outbox-backlog.md) |
| `TopupUnsupportedInflows`, rejected funds at the treasury | [Rejected funds at treasury](rejected-funds-at-treasury.md) |
| `TopupTreasurySanctioned`, a treasury on a sanctions list | [Treasury change, "Sanctioned treasury"](treasury-change.md#sanctioned-treasury) |
| `TopupDepositReversed`, `TopupDepositPendingAfterReorg`, `topup-finality-watch` | [Deposit reversed or pending after a reorg](deposit-reversed.md) |
| Merchant reports a secret key exposed or lost, or requests it did not make | [API key compromise and key recovery](api-key-compromise.md) |
| Unswept credited deposits, a `FlushFailed` target | Not a platform alert: the merchant sweeps with its own wallet, and a target whose transfer failed (a token or treasury refusing it) is the merchant's to resolve ([deploy/README.md, "Sweeping"](../README.md#sweeping)) |
| Database loss, restore drill | [RESTORE.md](../RESTORE.md) |
| After a restore: merchants get `503 service_restoring`, `GET /v1/admin/restore` shows `frozen` | [Reconciliation after a restore](restore.md) |
| A merchant's refund stays `pending` or `failed` | Not a platform action: the merchant pays refunds from the treasury of the deposit's address and attaches the transaction with `POST /v1/refunds/{id}/mark_paid`; a `failed` refund's `failure_reason` says why ([integration guide, §3](../../docs/integration.md#3-refunds)) |
| A merchant's treasury change, or a pending `treasury.created` it did not request | [Treasury change](treasury-change.md) |
| A treasury reported compromised: hold its payments uncredited | [Treasury crediting pause](treasury-credit-pause.md) |
| Removing a route version or a chain's last route | [Route or chain retirement](route-retirement.md) |
| On a USDT route: a nonzero fee (`basisPointsRate`, `maximumFee`) or a `Params` event, a `FlushFailed` with an empty `reason`, a forwarder or treasury on Tether's blacklist | [USDT fee switch and blacklist](usdt-issuer-controls.md) |
| Payment sent on another EVM chain | [Wrong-network deposit](wrong-network-deposit.md) |
| Any customer-impacting incident | [Incident communication](incident-communication.md) |

## Exercise status

The exercises of Phala's instance; an operator records its own. Local exercises ran against
PostgreSQL and Anvil with the `topup` CLI or the integration tests; Safe, Compliance, and
publication steps are human-only and were never exercised. Except the restore drills, every
exercise ran a database-level form of these runbooks. The full CVM restore drill of 2026-09-30 ran
[RESTORE.md](../RESTORE.md) and [Reconciliation after a restore](restore.md) as they were at
`e39b06b` (#247), in a drill app isolated from staging.

| Runbook | Last run | Outcome | Evidence |
|---|---|---|---|
| Chain frozen | 2026-09-22, local | complete: freeze, dual-provider check, owner lift, re-freeze | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Reconciliation mismatch | 2026-09-22, local | complete: findings, blocks, owner-only lift | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Lock exposure near cap | 2026-09-22, local | complete: cap enforcement; the alert itself not evaluated | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Restore | 2026-09-30 04:55–06:20 UTC, full CVM drill (drill app, own domain and prefix; Release images run 36669475654) | complete: the service provisioned on the release, archiving cut at 05:34:02 UTC by sealing invalid storage credentials into the source (a drill-only step, so later writes were lost), failure declared 05:38:20; a restore-check instance of the same app: attestation `UpToDate` for the original app id, report `ok`, reconciliation complete, restored heartbeat 05:33:51 (11 s before the cut; lost window 05:34:22–05:38:20, last segment 05:34:22); resumed to the service compose on the domain (TXT to the new instance, certificate evidence, 8081 closed), a new base backup and WAL on timeline 2 (no `.history` archived: archiving is off at promotion); unfrozen 06:17:04, **RTO 38 min 44 s**. Found and fixed in step 2: CLI 1.1.22 `cvms attestation` fails while the app has two instances. Deviations: read-write storage credentials on the restore instance (no read-only token), no Sentry DSN. The staging drill of 2026-09-25 ([#126](https://github.com/Phala-Network/phala-pay/pull/126)) ran steps 1–5 on staging's own backups | [#248](https://github.com/Phala-Network/phala-pay/pull/248) |
| Reconciliation after a restore | 2026-09-30, full CVM drill | complete except the security re-application: the merchant's records fetched from the reference product (`fetch-restore-records --since`); the lost deposit address (rotated after the restore point) and the lost quote re-issued with their ids, addresses, and client secrets; three signed deliveries imported (two `imported`, one `matches`), a tampered body refused; the rescan rebuilt both lost deposits under their original ids, nothing credited while frozen, no `contradicted` finding; after the unfreeze both credited at the delivered credits (the quote's at the quote price, the address's at the delivered spot rate), nothing delivered again, the receiver's balances unchanged, and a new quote paid, credited, and its webhook delivered. A lost test-mode treasury change verified `missing` (the treasury it replaced `differs`) and `treasuries/apply` refused its deliveries, now in step 3; the merchant proved it again after the unfreeze. Not exercised on a CVM: a key revoked after the restore point (the re-revoke answered for one revoked before it), a pending change restored by `treasuries/apply` (test-mode changes apply at once: `restore_mode.rs` and `make restore-drill`), deleted endpoints, reversed deposits; merchant contact | [#248](https://github.com/Phala-Network/phala-pay/pull/248) |
| Lock expiry worker failure | 2026-09-22, local | partial: exercised a counter since removed | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Provider disagreement | 2026-09-22, local | partial: sanctions truth table; no disagreeing-provider fixture | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Price outage | 2026-09-22, local | partial: route pause; no controllable price-source fixture | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Rejected funds at treasury | 2026-09-22, local | partial: report; Compliance and Safe steps human-only | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Treasury change | 2026-09-22, local | partial: tooling refuses without Safe expectations; the rest is human-only | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Outbox backlog | 2026-09-22, local | partial: no controllable webhook receiver | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Scanner lag | 2026-09-22, local | partial: no controllable finalized-chain fixture | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Backup age | 2026-09-22, local | partial: predates encrypted backups; local restore drills cover archiving | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Incident communication | 2026-09-22, local | partial: publication and roles human-only | [#59](https://github.com/Phala-Network/phala-pay/pull/59) |
| Wrong-network deposit | — | not exercised; every step is human-only | — |
| API key compromise and key recovery | 2026-09-28, local | partial: the merchant roll and the operator recovery in the API tests `keys_authenticate_by_bearer_and_expire_or_revoke` and `operator_onboards_accounts_enables_live_mode_and_recovers_keys`; the contact verification not exercised | [#191](https://github.com/Phala-Network/phala-pay/pull/191) |
| Route or chain retirement | — | not exercised; the upgrade is human-only | — |
| Treasury crediting pause | 2026-09-28, local | partial: merchant and operator pause and resume, and the held deposit, in the API test `crediting_pauses_per_treasury_and_resumes`; the contact verification not exercised | [#203](https://github.com/Phala-Network/phala-pay/pull/203) |
