# Changelog

All notable changes to `crypto-topup-sdk` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the rules in `docs/sdk.md`; versions
follow [Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- Generated `topup_client.api.admin.lift_reconciliation_block` and `replay_outbox_event` with
  `AdminReasonRequest`, `ReconciliationBlockLiftResponse`, and `OutboxReplayResponse`.
- `DailyReportResponse.reconciliation_blocks` (`ReconciliationBlockReport`) and
  `SupportDepositResponse.events` (`DepositEventResponse`). Optional, so the models also parse
  responses from servers that predate them.
- `DailyReportResponse.reconciliation` (`ReconciliationRoundReport`, `FailedCheckReport`) and
  `RouteDailyReport.flush_planning` (`FlushPlanningReport`): the latest reconciliation round and
  scheduled flush planning run of the serving process. Optional, so the models also parse
  reports from servers that predate them.
- `AttestationResponse.operators` (`OperatorIdentity`): the flusher operator of each configured
  chain, bound into `report_data`. Optional, so the model also parses responses from servers
  that predate it.
- Generated `topup_client.api.admin.register_product` with `RegisterProductRequest` and
  `ProductResponse` for `POST /v1/admin/products`.
- `topup_sdk.verify_attestation_binding` and `attestation_report_data`, which check that
  `report_data` binds the nonce, the settlement key, and every listed operator, and
  `AttestationError`.

### Changed

- `TopupClient.attestation` raises `AttestationError` unless `report_data` binds the returned
  keys. It does not verify the quote itself.

### Removed

- **Breaking:** the generated `ErrorDetail.work_package` field and the `501` response of
  `/v1/attestation` (`get_attestation`) (#90). Both belonged only to the pre-C11 placeholder
  attestor; the production service never returned them. There was no deprecation window because
  neither was ever reachable in production.

### Added

- `TopupClient.list_pending_deposits` and the generated `list_pending_deposits` operation with
  `PendingDepositResponse` and `PendingDepositsResponse`: transfers to persistent addresses seen
  before finality. Display only; they are not credited.
- `RateLockResponse.payment` (`RateLockPayment`): the payment the checkout page should show (the
  consuming deposit, else the first qualifying payment, else the first), `seen` before finality
  or `finalized` once it is a deposit.

### Changed

- The service now rebuilds `@target-uri` from its configured public origin
  (`TOPUP_PUBLIC_ORIGIN`) instead of the `Host` and `X-Forwarded-Proto` headers, so requests
  signed for the public URL verify behind the dstack gateway (#77). The `http_message_signature`
  security scheme in `openapi.json` documents this. No SDK code changed.

### Added

- `DailyReportResponse.exposure_minor`, the global open rate-lock credit in destination minor
  units (#94). Optional, so the model also parses reports from servers that predate it.

### Removed

- **Breaking:** `RouteDailyReport.exposure_minor`, `exposure_minor_reason`, `pnl_minor`, and
  `pnl_minor_reason` (#94). They were always null placeholders on the admin-only daily report;
  PnL is not defined precisely enough in the design to compute. Allowed as a pre-GA exception:
  no service has been deployed. Regenerated models only.

## 0.1.0 - 2026-09-22

Generated from OpenAPI `info.version` 0.1.0.

### Added

- Generated `topup_client` package for the product and administrative API.
- RFC 9421 ed25519 request signing (`RequestSigner`, `SigningAuth`) with a random `nonce` per
  signature, and inbound verification (`verify_request`) in the service's profile.
- Standard Webhooks `v1a` verification (`verify_webhook`, `verify_webhook_signature`).
- Deposit-id, salt, and CREATE2 forwarder-address recomputation.
- `TopupClient` with idempotent, retrying helpers for accounts, deposit addresses, rotation,
  rate locks, deposits, limits, refunds, and attestation.
- `topup-sdk keygen` and `topup-sdk public-key` for credential issuance.
