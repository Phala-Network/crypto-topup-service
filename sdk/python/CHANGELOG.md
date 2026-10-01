# Changelog

<!-- markdownlint-disable-file MD024 -->

All notable changes to `phala-pay` (formerly `crypto-topup-sdk`) are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the rules in `docs/integration.md`
(section 5.9); versions follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.3.0] - 2026-10-01

### Changed (breaking)

- `Webhook.construct_event` takes webhook public keys only in Standard Webhooks' `whpk_` form
  (`whpk_` and the standard base64 of the key), as `GET /v1/attestation` lists them; a hex or bare
  base64 key is refused. `load_webhook_public_key` and `webhook_public_key_bytes` parse that form.
- `load_public_key` parses a request-signing key only, the standard base64 of its raw bytes
  (`RequestSigner.public_key_base64`).
- `WebhookKeyObject.public_key` is the key in the `whpk_` form, and
  `standard_webhooks_public_key` is gone (`topup_client`); `verify_attestation_binding` decodes
  each key from it.
- `Deposit.receipt_log_index`, `revision`, `block_hash`, and `block_time` are required
  (`topup_client`).
- `ClientQuote.confirmations` and `ClientDepositAddressPayment.confirmations` are required, an
  integer or `None` (`topup_client`), as the service always sends them.
- `Webhook.construct_event` requires the event's `actor` and `request` (`null` or the causing
  request), as the service always sends them, and refuses an envelope without them. `Event` gains
  `actor`, and its fields are in envelope order: `request` follows `actor` and has no default.

### Added

- `Deposit.replaces` and `replaced_by` (`topup_client`): the deposit a reorganization's new
  transfer at the same receipt position replaced, and the reverse link.
- `topup-sdk keygen` and `public-key` also print `webhook_public_key`, the key in the `whpk_`
  form, for a test instance's webhook key.

### Fixed

- `topup-sdk send-test-event` sends a deposit with every required field (`swept`, its receipt
  position, revision, and block), so `construct_event` accepts it.

- The client module's docstring no longer says nothing is credited until a deposit is final: a
  deposit is credited at its route's confirmations, within the account's
  `max_unfinalized_credit`, and a reorg before finality reverses it.

## [0.2.0] - 2026-09-29

The first release on PyPI.

### Added

- `list_forwarders` / `pay.forwarders.list` take the API's `quote` and `deposit_address` filters.
- `Deposit.final_at`; `Config.max_open_quotes` and `max_open_amount_per_customer`
  (`max_open_amount_per_account` is now the account's cap in the mode).

### Fixed

- Error responses are read leniently: a body without `doc_url` (or any field but `code`), with an
  unknown `type`, or not JSON at all (a proxy's `502` page) raises `ApiError` (`unexpected_response`
  when there is no error object), never `KeyError` or `JSONDecodeError`, and a retryable status
  is retried whatever its body.

### Changed (breaking)

- Address checks derive every address from your pins, never from the response's `treasury`: a
  quote or deposit address network naming another treasury than your pinned one of its chain
  raises `AddressMismatchError`. With a live key (`ppay_sk_live_`, `ppay_rk_live_`) the check is
  mandatory and fails closed unless `account`, `forwarder`, and `treasuries` are all given; in test
  mode an unpinned treasury falls back to the response's with an `UnpinnedTreasuryWarning`.
- `roll_webhook_key` / `pay.account.roll_webhook_key` default to `expires_in=172800`, the shortest
  overlap a live roll accepts (was `0`).

- `Deposit` has `amount_refunded` and `amount_reversed`, the service-computed cumulative
  claw-backs; `Refund.receipt_log_index` replaces `log_index`, and `mark_paid(…,
  receipt_log_index=…)` / `mark_refund_paid(…, receipt_log_index=…)` replace `log_index=`. A refund
  marked paid can no longer be canceled. The FastAPI example applies every `deposit.*` event by
  the balance rule (docs/integration.md §2.3).

- API conformance with Stripe (docs/design/multi-tenant.md, "API conformance"): business-state
  failures such as `deposit_not_final`, `quote_unexpected_state`, `paused`, `treasury_not_set`,
  and `*_cap_exceeded` are `400` (only `idempotency_key_in_use` is `409`); per-customer limits are
  `429 customer_rate_limit`; every `429` carries `Retry-After`, which the client waits.
- `ApiError.request_id` reads only `Request-Id`; `X-Request-Id` is no longer sent. `ApiError`
  gains `doc_url` and `retry_after`.
- A response the service saved for an `Idempotency-Key` (now every executed request, even a
  `500`) arrives `Idempotent-Replayed` and is raised, not retried.
- A quote's address salt is `keccak256(abi.encode(account, client_reference_id, "quote",
  quote_id))` (design D3; it was tagged `"lock"`): `topup_sdk.quote_salt` replaces `lock_salt`.
- Treasury events are `treasury.created`, `treasury.updated`, and `treasury.canceled` (were
  `account.treasury.pending|updated|canceled`).
- The generated client has no admin API (the operator's is `openapi.admin.json`); every object's
  `object` is a `Literal` (`literal_enums`), and so is `ErrorDetail.type_`.

- The API's name for your customer is `client_reference_id` everywhere: `pay.quotes.create(
  client_reference_id=…)`, `pay.deposits.list(client_reference_id=…)`, `Quote`/`Deposit`
  `.client_reference_id`, `CreditedDeposit.client_reference_id`, and `topup-sdk send-test-event
  --client-reference-id`.
- `PhalaPay(…, forwarder=(factory, implementation))` is required and a pair, and
  `treasuries={chain_id: treasury}` pins the treasury each address is derived from (required in
  live mode, above). `TopupClient(forwarder=)` takes the same pair.
- A deposit's `status` is `pending`, `credited`, `rejected`, or `reversed`, with booleans `final`
  and `swept`; `CreditedDeposit` accepts only `credited`.
- `Quote.payment` is the shared `Payment` model (`status` `seen` or `recorded`, with `chain_id` and
  `asset`); `QuotePayment` is gone. `DepositAddress.payments` uses the same model.
- `AttestationResponse.tdx_quote` replaces `.quote`.
- Every object's `livemode` and `metadata` are required in the generated models.

### Added

- Restricted keys: `TopupClient`/`PhalaPay` accept `ppay_rk_…` keys, and
  `create_api_key(name=, permissions=[...])` / `pay.api_keys.create(permissions=)` create one.
- `pause_treasury` / `resume_treasury` (`pay.treasuries.pause`, `.resume`): the merchant's
  crediting pause of a treasury. `topup_sdk.UnpinnedTreasuryWarning`.

- Events are snapshots with their cause: `Event.request` (`EventRequest`: the `Request-Id` and
  `Idempotency-Key` of the request that caused it, `None` for the service's workers) and
  `EventData.previous_attributes` on `*.updated` events; new events `refund.created`,
  `refund.updated`, and `quote.canceled`.
- `pay.events.list(types=, delivery_success=, created_gt=, created_gte=, created_lt=,
  created_lte=)`, `pay.deposits.list(created_gt=, created_lt=)`, and every page of
  `pay.api_keys.list()` and `pay.treasuries.list()`. Webhook endpoints carry
  `pending_deliveries`, `oldest_pending_at`, and `last_attempt`.
- `Literal` hints: `phala_pay.QuoteStatus`, `DepositStatus`, `DepositAddressStatus`,
  `RefundStatus`, `TreasuryStatus`, `ApiKeyStatus`, `WebhookEndpointStatus`, `PaymentStatus`, and
  `EventType`.
- `pay.account` (`retrieve`, `update(confirmation_policies=)`, `pause_quotes`, `resume_quotes`,
  `roll_webhook_key`), `pay.api_keys`, `pay.webhook_endpoints`, `pay.events` (`list`, `retrieve`,
  `resend`), `pay.treasuries` (`challenge`, `create`, `set_eoa`, `list`, `retrieve`, `cancel`),
  `pay.balance`, `pay.sweeps`, `pay.forwarders`, `pay.quotes.list`, `pay.refunds.list`, and
  `pay.export_account(directory)`, with the matching `TopupClient` methods.
- `ApiError.request_id`, read from the response's `Request-Id`.
- Offline sweeping: `topup_sdk.flush_transaction(factory, treasury, salts, token)`,
  `flush_transactions(forwarders, token)`, `safe_batch(chain_id, safe, calls)` writing the Safe
  Transaction Builder's `BatchFile` JSON with the app's checksum, `write_safe_batch`, and
  `batch_checksum`.
- `topup_sdk.sign_treasury_challenge(message, private_key, address=)`, the EIP-191 proof of an EOA
  treasury (extra `phala-pay[eoa]`), and `topup_sdk.quote_address(...)`.
- `DepositAddress.client_secret` and `.payments`; the generated client reads a deposit address's
  public `ClientDepositAddress`.

- The generated client covers treasuries (design D10): `topup_client.api.treasuries`
  (`create_treasury_challenge`, `create_treasury`, `list_treasuries`, `get_treasury`,
  `cancel_treasury`) and the `Treasury`, `TreasuryChallenge`, and `TreasuryList` models; `Quote`
  gains the optional `treasury`; `Treasury` carries `cancellation_reason`, and the admin client
  gains `pause_account` and `resume_account`. Challenge and submit helpers for EOAs and Safes come with the SDK
  work of design PR 10. The generated `RouteDailyReport` drops the treasury balance fields.
- `metadata`: `pay.quotes.create(…, metadata=)` and `pay.refunds.create(…, metadata=)`, and
  `pay.quotes.update(id, metadata=)`, `pay.deposits.update(id, metadata=)`, and
  `pay.refunds.update(id, metadata=)`, which merge (a key set to `""` is unset, `metadata=""`
  unsets all); `TopupClient` gains the same `metadata=` and `update_quote`, `update_deposit`, and
  `update_refund`. The regenerated `Quote`, `Deposit`, and `Refund` carry `metadata`, so a webhook
  deposit carries its quote's.
- Deposit addresses, one per customer for every supported token on every chain:
  `PhalaPay.deposit_addresses.create(client_reference_id=, metadata=)`, `.retrieve(id)`,
  `.list(client_reference_id=, status=)`, `.update(id, metadata=)`, and `.rotate(id)`, with the
  matching `TopupClient` methods and the generated `DepositAddress`, `DepositAddressNetwork`, and
  `DepositAddressAsset` models. With a pinned `forwarder`, every network of an active deposit
  address must pay the pinned treasury at the recomputed address, or `AddressMismatchError` is
  raised. `topup_sdk.deposit_address_salt(account, livemode=, client_reference_id=, version=)` and
  `topup_sdk.deposit_address(factory, implementation, treasury, …)` recompute any version offline
  from a network's treasury; `Deposits.list` and `TopupClient.list_deposits` take
  `deposit_address`, and `Deposit.deposit_address` names the address a deposit reached.

- Generated `topup_client.api.webhook_endpoints` (create, list, retrieve, update, delete, test)
  and `topup_client.api.events` (list, retrieve, resend) with their models
  (`WebhookEndpointObject`, `EventObjectResponse`, and the request and list types); the
  hand-written `PhalaPay` helpers for them come with design PR 10.
- Generated `CreateAccountRequest` and `UpdateAccountRequest` lose `webhook_url`, and
  `topup_client.api.admin.replay_outbox_event` and `OutboxReplayResponse` are removed: the
  operator no longer manages merchants' webhooks.

- `TopupClient.roll_webhook_key(expires_in=)` rolls the mode's webhook key (one
  `Idempotency-Key` across retries). `sign_webhook` accepts several keys, as a rotation signs.

### Changed

- **Breaking**: webhooks are signed with your account's key per mode (design D11).
  `Webhook.construct_event(payload, headers, public_key, expected_account, *, expected_livemode)`
  and `verify_webhook(headers, body, public_keys, *, expected_account, expected_livemode)` fail
  closed unless a signature verifies with a pinned key (one key, or a list while a rotation
  overlaps) and the event's `account` and `livemode` match; `Event` and `WebhookEvent` carry
  `account` and `livemode`. Events in the envelope before `evt_` ids are refused.
  `attestation_report_data(nonce, account, livemode, keys)` computes the new binding and
  `verify_attestation_binding(response, nonce, *, expected_account=, expected_livemode=)` returns
  the attested public keys; `AttestationResponse` has `account`, `livemode`, and `webhook_keys`
  instead of `keyid` and `settlement_pubkey`, and `TopupClient.attestation` sends the API key.
  `topup-sdk send-test-event` takes `--account` and also checks that another account's event is
  refused. `Quote`, `Deposit`, `Refund`, `Config`, and `AccountObject` gain `livemode` or
  `webhook_keys`.
- **Breaking**: refunds are paid by the merchant from the refund's `treasury` and attached with
  `TopupClient.mark_refund_paid(refund_id, transaction_hash, log_index=)` (`pay.refunds.mark_paid`);
  `cancel_refund` (`pay.refunds.cancel`) cancels a pending one. `Refund` gains `treasury`,
  `failure_reason`, and `log_index`, renames `tx_hash` to `transaction_hash`, and its `status` may
  also be `failed` or `canceled`. A failed refund is announced by the `refund.failed` webhook,
  whose `data.object` is the refund. The regenerated admin client drops `approve_refund`,
  `record_refund`, `AdminRefundResponse`, and `RecordRefundRequest`.

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

Not published.

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

[Unreleased]: https://github.com/Phala-Network/phala-pay/compare/sdk-py-v0.3.0...HEAD
[0.3.0]: https://github.com/Phala-Network/phala-pay/compare/sdk-py-v0.2.0...sdk-py-v0.3.0
[0.2.0]: https://github.com/Phala-Network/phala-pay/releases/tag/sdk-py-v0.2.0
