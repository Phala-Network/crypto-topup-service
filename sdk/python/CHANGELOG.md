# Changelog

All notable changes to `phala-pay` (formerly `crypto-topup-sdk`) are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the rules in `docs/integration.md`
(section 5.9); versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Changed

- **Breaking**: the service sends no transactions, so attestation binds no flusher operators.
  `AttestationResponse` drops `operators` (and `OperatorIdentity` is gone), `report_data` is
  `sha256(nonce ‖ settlement_pubkey)`, and `attestation_report_data(nonce, settlement_pubkey)`
  takes no operators. The admin daily report drops `flush_planning` (`FlushPlanningReport`), and
  its `unflushed_balance_atomic` is what forwarders still hold: deposits not reversed minus
  finalized `Flushed` amounts, whoever flushed.
- **Breaking**: merchant requests carry a secret key, `Authorization: Bearer ppay_sk_…`, instead
  of an RFC 9421 signature (design D7, PR 5). `TopupClient(base_url, api_key, *, account=None,
  forwarder=None, …)` and `PhalaPay(api_base, api_key, *, account=None, forwarder=None, …)`
  replace the signer and key file arguments; the address check reads the account id once from
  `GET /v1/account` unless `account=` is given. `TopupClient.get_account()` and `account_id()`
  are new. The client retries `409 idempotency_key_in_use` instead of `signature_replayed`.
  `RequestSigner` stays for the operator's admin requests.
- The regenerated client adds `/v1/account`, `/v1/api_keys` (create, list, retrieve, roll,
  revoke), and the admin `create_account` (contact, due diligence, `charges_enabled`, first keys),
  `update_account` (now `POST`), and `issue_api_key`; customer pauses take `livemode`.

- **Breaking**: products are gone; the service's tenant is an account, `acct_…`. Sign with the key
  id the operator issued with your account, `{acct_…}/v1`: the client takes the account id from
  it as the first input of every quote's address salt, as it took the product slug. The
  regenerated admin client replaces `register_product`, `update_product`, `pause_account`, and
  `resume_account` with `create_account`, `update_account`, `pause_customer`, and
  `resume_customer`; the admin deposit view carries `account` and `livemode`.

- **Breaking**: forwarders are clones of the new permissionless factory whose address commits to
  the treasury. `forwarder_address(factory, implementation, treasury, salt)` takes the treasury,
  and `forwarder=` is the `(factory, implementation, treasury)` triple.

### Added

- Fast credit: the regenerated client carries the config's `confirmations` and
  `typical_credit_seconds`, and the admin deposit view's `receipt_log_index` and `final_at`.
  `Event` documents `deposit.reversed`: claw its credit back as for `deposit.refunded`.

### Changed

- **Breaking**: `topup_sdk.deposit_id(chain_id, tx_hash, receipt_log_index)` hashes the
  transfer's position in its transaction's receipt (0 for a plain token transfer), the service's
  deposit identity since fast credit; the argument was the block-wide `log_index`.

- `phala_pay`: the Stripe-style facade. `PhalaPay(api_base, key_id, key_file=… | seed=…,
  forwarder=…)` with `quotes.create/retrieve/cancel`, `deposits.list` (auto-paginating) and
  `retrieve`, `refunds.create/retrieve`, and `config.retrieve`; `webhooks.construct_event(payload,
  headers, public_key)` verifies a delivery and returns a typed `Event` (`data.object` is a
  `Deposit` or `Quote`), raising `SignatureVerificationError`.
- `TopupClient.create_refund` (sends an `Idempotency-Key`) and `get_refund`; `topup_sdk.ids`
  (`object_id`, `parse_id`, and the `qt_`/`dep_`/`re_`/`evt_` prefixes).
- `TopupClient.get_config`, `create_quote` (sends an `Idempotency-Key`, generated unless given,
  and reuses it on retries), `get_quote`, and `cancel_quote`; `topup_sdk.signing.sf_string`.
- `Quote.client_secret`, returned by `create_quote` only, and the generated `ClientQuote` model:
  the public view the payer's browser reads from `GET /v1/quotes/{id}?client_secret=…`.

- `topup_sdk.fulfillment`: `CreditedDeposit.from_event` parses a verified `deposit.credited`
  into a typed credit with its `fulfillment_key` (`deposit:<deposit_id>`), raising
  `FulfillmentError` for any other shape; `credited_event_id` derives the event's
  `webhook-id` from the deposit id; `CREDITED_EVENT`. The service emits this payload once
  `deposit.credited` becomes the fulfillment event (docs/integration.md §2).
- `topup_sdk.sign_webhook` (test senders) and `topup-sdk send-test-event`, which checks a webhook
  receiver answers a signed event and its duplicate with `2xx` and a forged copy with `4xx`.
- `DepositResponse.external_id` and `price_source` (also on `SupportDepositResponse`).
- Generated `topup_client.api.admin.update_product` with `UpdateProductRequest` for
  `PUT /v1/admin/products/{slug}`.
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
- `TopupClient.list_pending_deposits` and the generated `list_pending_deposits` operation with
  `PendingDepositResponse` and `PendingDepositsResponse`: transfers to persistent addresses seen
  before finality. Display only; they are not credited.
- `RateLockResponse.payment` (`RateLockPayment`): the payment the checkout page should show (the
  consuming deposit, else the first qualifying payment, else the first), `seen` before finality
  or `finalized` once it is a deposit.
- `DailyReportResponse.exposure_minor`, the global open rate-lock credit in destination minor
  units (#94). Optional, so the model also parses reports from servers that predate it.

### Changed

- **Breaking**: the distribution is `phala-pay` (was `crypto-topup-sdk`), published to
  PyPI from `sdk-py-v*` tags. The `topup_sdk` and `topup_client` imports are unchanged.
- **Breaking**: `verify_webhook` parses Stripe-style events: `WebhookEvent(id, type, created, data)`
  with `object` (the event's `data.object`), still accepting the old envelope of a replayed old
  event. `CreditedDeposit` carries the deposit's fields (`deposit_id` `dep_…`, `account_id`,
  `amount` in cents, `price_source` `quote` or `spot`, `quote`, …) and its `fulfillment_key` is
  the `dep_` id; `deposit_id` returns `dep_…` and `credited_event_id` `evt_…`.
  `topup-sdk send-test-event` takes `--account-id` and `--amount` and sends the new envelope.
- **Breaking**: `TopupClient(base_url, signer, *, forwarder=None, …)`: the product is the signer's
  key id, `{product}/v1`; there is no `product_slug` argument. With `forwarder=(factory,
  implementation)` pinned, `create_quote` and `get_quote` recompute an open quote's address and
  raise the new `AddressMismatchError`.
- **Breaking**: `ApiError` carries the error object's `error_type` and `param`.
- `TopupClient.attestation` raises `AttestationError` unless `report_data` binds the returned
  keys. It does not verify the quote itself.
- The service now rebuilds `@target-uri` from its configured public origin
  (`TOPUP_PUBLIC_ORIGIN`) instead of the `Host` and `X-Forwarded-Proto` headers, so requests
  signed for the public URL verify behind the dstack gateway (#77). The `http_message_signature`
  security scheme in `openapi.json` documents this. No SDK code changed.

### Removed

- **Breaking**: `create_deposit_address`, `get_deposit_address`, `rotate_deposit_address`,
  `list_pending_deposits`, and `topup_sdk.persistent_salt`, with their generated operations and
  models: quotes are the only flow.
- **Breaking**: `lookup_deposits` and `request_refund`; `list_deposits(external_id, state=…,
  created_from=…, created_to=…)` becomes `list_deposits(account_id=…, quote=…, status=…,
  tx_hash=…, created_gte=…, created_lte=…, expand=…)` over Stripe's cursors, and `get_deposit`
  takes a `dep_` id. Use `create_refund` and `get_refund`.
- **Breaking**: `register_account`, `create_rate_lock`, `get_rate_lock`, `cancel_rate_lock`, and
  `get_limits`, with their generated `topup_client` operations and models. Use `create_quote`,
  `get_quote`, `cancel_quote`, and `get_config`.
- **Breaking:** the generated `RouteDailyReportSettlementsByStatus` and
  `RouteDailyReport.settlements_by_status`, replaced by `credited_undelivered` and
  `credited_undelivered_max_age_seconds`; the settlement-request test vectors, since the service
  no longer sends settlement requests.

- **Breaking:** the generated `ErrorDetail.work_package` field and the `501` response of
  `/v1/attestation` (`get_attestation`) (#90). Both belonged only to the pre-C11 placeholder
  attestor; the production service never returned them. There was no deprecation window because
  neither was ever reachable in production.
- **Breaking:** `RouteDailyReport.exposure_minor`, `exposure_minor_reason`, `pnl_minor`, and
  `pnl_minor_reason` (#94). They were always null placeholders on the admin-only daily report;
  PnL is not defined precisely enough in the design to compute. Allowed as a pre-GA exception:
  no service has been deployed. Regenerated models only.

## [0.1.0] - 2026-09-22

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
