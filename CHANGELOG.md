# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the SDKs keep their own changelogs in
`sdk/js` and `sdk/python`.

## [Unreleased]

### Added

- Accounts and tenancy (docs/design/multi-tenant.md §14, D13, PR 3). The tenant is an account,
  `acct_…`. Its request signing key id is `{acct_…}/v1`, and the key is live or test: it quotes on
  the routes of its mode and reads only its mode's objects. Another account's object, or the same
  account's object in the other mode, answers `404` like a missing one. Webhook events go to every
  enabled endpoint of the event's account and mode.
- `POST /v1/admin/accounts {name, livemode, public_key, webhook_url}` issues an account and
  `PUT /v1/admin/accounts/{account}` replaces its key and webhook URL;
  `POST /v1/admin/accounts/{account}/customers/{customer}/pause | resume` pauses one customer.
  The admin deposit view carries the deposit's `account` and `livemode`.

- Fast credit and reversal (docs/design/multi-tenant.md §4, D1). A route's
  `chain.confirmations` (a depth, `safe`, or `finalized`, per chain family; default 2 on
  Ethereum L1, `safe` on OP-stack, `finalized` elsewhere) sets when a deposit is credited: at the
  default, `deposit.credited` is sent about 30 seconds after paying instead of about 15 minutes.
  Deposits are watched to finality. A transaction re-included in another block keeps its deposit
  and is followed; one proven dropped (its nonce consumed by another transaction), or whose
  transfer is missing from its final receipt, makes the deposit `reversed` and sends
  **`deposit.reversed`** (event id `uuid_v5(NS, "deposit.reversed:" + deposit UUID)`) when the
  deposit was reported credited or rejected. Claw the credit back as for `deposit.refunded`. A
  quote the deposit completed opens again while its window lasts, otherwise expires with
  `quote.expired`. `confirmations: finalized` keeps the earlier behaviour.
- `GET /v1/config` assets carry `confirmations` (`"2"`, `"safe"`, or `"finalized"`) and
  `typical_credit_seconds`; the admin deposit view carries `receipt_log_index` and `final_at`.
- `POST /v1/refunds` answers `409 deposit_not_final` for a deposit that is not final yet, so
  nothing is paid back for a payment that could still be reversed.

### Changed

- **Breaking:** the service sends no transactions (docs/design/multi-tenant.md §5, §13, PR 4).
  Anyone, usually the merchant with its own wallet or Safe, sweeps forwarders with the
  permissionless factory's `flush(treasury, salts, token)` and pays the gas. The finalized scanner
  indexes the factory's `ForwarderCreated`, `Flushed`, and `FlushFailed` events for the account's
  own `(address, treasury)` pairs, whoever sent them, and a final credited deposit becomes `swept`
  once a finalized `Flushed` event follows it. A `FlushFailed` target keeps its balance and its
  deposits stay `credited`. Reconciliation compares every active forwarder's finalized balance
  with its deposits minus its finalized sweeps; a mismatch freezes crediting on the chain
  (`409 chain_frozen`) until the operator lifts it.
- **Breaking:** attestation binds only the settlement key: `GET /v1/attestation` drops
  `operators`, `report_data` is `sha256(nonce ‖ settlement_pubkey)`, and `topup attest` drops
  `--route` and `--operator-key-version` and its `operators`, `operator_keyid`, and
  `operator_address` fields.
- **Breaking:** route files drop `chain.operator_key_version`, `chain.flush`,
  `limits.min_flush_atomic`, and `alerts.stuck_after_s.credited` (a credited deposit waits for
  its merchant's sweep); the pause scope `flush` is gone. The daily report drops `flush_planning`,
  and its `unflushed_balance_atomic` is deposits not reversed minus finalized `Flushed` amounts.
- **Breaking:** one squashed database migration builds the multi-tenant schema on an empty
  database; staging is reset (HUMAN-ONLY, deploy/README.md "Staging reset") and nothing is
  migrated. Route files drop `product` and require `livemode`, checked against the chain.
- **Breaking:** products are gone. `POST /v1/admin/products`, `PUT /v1/admin/products/{slug}`,
  and `POST /v1/admin/products/{slug}/accounts/{account_id}/pause | resume` are replaced by the
  account endpoints above; the quote address salt's first input is the `acct_…` id instead of the
  product slug.
- A quote's `account_id` holds 1 to 200 characters (was 255 bytes).

- **Breaking** for deposits recorded from now on: a deposit id is `uuid_v5(NS,
  "{chain_id}:{tx_hash}:{receipt_log_index}")`, the transfer's position among its transaction's
  receipt logs (0 for a plain token transfer), instead of the block-wide `log_index`, so a
  re-included transaction keeps its id. `log_index` and `block_number` stay on the deposit as
  evidence and change when the transaction is re-included. Deposits recorded before keep their
  ids; staging is reset before multi-tenancy.
- Deposit `status` gains `reversed`; the quote's `payment.status` `final` now means recorded at
  the route's confirmation, and the payer's `payment_status` `confirming` likewise.

- `POST /v1/products/{p}/accounts/{ext}/rate-locks` and `POST …/deposit-address` create the
  account when it does not exist, so a quote or an address is one call; `POST …/accounts` is no
  longer required. Reads (`GET`, rotate, cancel, pause) of an unknown account still answer `404`.

- `DepositResponse` (deposits, deposit, and support lookup) carries `external_id`, the account
  of the receiving address, and `price_source` (`lock` or `spot`).

- `PUT /v1/admin/products/{slug} {public_key, webhook_url, reason}` (administrative API) replaces
  an issued product's verification key and webhook URL, which `POST /v1/admin/products` refuses
  with `409`. The key id stays the route's `destination.product_kid`. The cut is immediate: the
  old key stops verifying when the change commits, with no overlap. The `audit` row
  (`product.update`) carries the reason and the replaced values; a repeat with the stored values
  changes nothing. An unknown slug is `404`, an unrouted slug or invalid value `400`.

- `POST /v1/admin/reconciliation-blocks/{block_key}/lift {reason}` lifts a reconciliation block
  (`chain:{chain_id}` or `address:{address_id}`), which production could not do without a database
  owner session. Lifting is manual: the reconciler blocks again if the finding still reproduces.
  The `audit` row (`reconciliation_block.lift`) carries the reason and the removed block; a repeat
  returns the first lift, and a key that never blocked is `404`. `GET /v1/admin/report/daily`
  lists the active `reconciliation_blocks`.

- `POST /v1/admin/outbox/{event_id}/replay {reason}` queues an existing webhook event for
  delivery again with the same id and payload (audited as `outbox.replay`); a repeat while the
  event is due changes nothing. The product-signed support lookup
  (`GET /v1/products/{p}/deposits?tx_hash=|address=|lock_ref=`) lists each deposit's webhook
  `events` (`id`, `event_type`, `created_at`, `delivered_at`).

- `GET /v1/admin/report/daily` returns `exposure_minor`, the global open rate-lock credit in
  destination minor units (#94). The field is optional in the schema so clients also parse reports
  from servers that predate it.

### Changed

- **Breaking**: webhooks are Stripe's Event object, `{"id": "evt_…", "object": "event", "type",
  "created", "data": {"object": …}}`, where `data.object` is the deposit (`deposit.credited`,
  `deposit.rejected`, `deposit.refunded`) or the quote (`quote.expired`, which replaces
  `rate_lock.expired`) as the API returns it, rendered at the first delivery attempt. The
  `webhook-id` is the `evt_` id, derived for every type from the event type and its object, so
  every re-emission deduplicates. `deposit.pending` and `deposit.confirmed` are no longer sent:
  the quote's `payment` shows a transfer before finality. Events delivered before this change
  keep their old envelope when replayed. The admin outbox replay and deposit view take and show
  `evt_` ids.
- **Breaking**: quotes replace rate locks (docs/architecture.md §9, §12). `POST /v1/quotes
  {account_id, amount, currency, chain_id, asset}`, `GET /v1/quotes/{id}`, and
  `POST /v1/quotes/{id}/cancel` replace `…/accounts/{ext}/rate-locks[/{ref}]`; the quote id
  (`qt_…`) replaces `product_lock_ref`, `Idempotency-Key` makes creation safe to retry, amounts
  are integer cents with `currency: "usd"`, timestamps are Unix seconds, and statuses are `open`,
  `complete`, `expired`, and `canceled`. A new quote's address salt uses its id as the reference.
  Quoting by token amount is removed. `GET /v1/config` lists the payable assets, limits, and
  quote terms; `POST …/accounts` and `GET …/accounts/{ext}/limits` are removed (the first quote or
  address creates the account).
- **Breaking**: quotes are the only flow. `GET|POST …/accounts/{ext}/deposit-address`,
  `…/deposit-address/rotate`, `GET …/accounts/{ext}/pending-deposits`, and the `addresses` pause
  scope are removed. Persistent addresses issued before stay watched by the finalized scanner;
  their payments are credited at spot, with `quote: null` on the deposit.
- **Breaking**: deposits and refunds are top-level resources. `GET /v1/deposits` (a Stripe list
  object with `starting_after`/`ending_before`/`limit` and filters `account_id`, `quote`, `status`,
  `tx_hash`, `created[gte|lte]`) and `GET /v1/deposits/{id}` return `Deposit` objects (`dep_` ids,
  `status`, `amount` in cents, `exchange_rate`, `price_source` `quote` or `spot`, `quote`,
  `amount_refunded_atomic`, `refunded`, Unix timestamps); `POST /v1/refunds {deposit,
  destination_address, amount_atomic?}` with `Idempotency-Key` and `GET /v1/refunds/{id}` return
  `Refund` objects (`re_` ids, status `pending` or `succeeded`). `expand[]` expands a deposit's
  `quote`, a quote's `deposit`, and a refund's `deposit`. The old deposit list, deposit, support
  lookup, and refund-request paths are removed; the operator's `GET /v1/admin/deposits/{id}` shows
  a deposit's transitions and webhook events, and account pause and resume move to
  `POST /v1/admin/products/{slug}/accounts/{account_id}/pause|resume`.
- `POST /v1/quotes` returns a `client_secret`, like Stripe's PaymentIntent. The payer's browser
  reads the quote's public view, `ClientQuote`, from `GET /v1/quotes/{id}?client_secret=…` without
  a signature (any origin; rate-limited). Only the secret's hash is stored: `GET` returns `null`,
  and a repeat with the same `Idempotency-Key` returns a new secret.
- **Breaking**: the product is identified by the request signature's key id, `{product}/v1`,
  not by the path.
- **Breaking**: errors are Stripe's error object, `{"error": {"type", "code", "message",
  "param"}}`, with Stripe-style codes (`parameter_invalid`, `resource_missing`,
  `signature_invalid`, `rate_limit`, `idempotency_key_reused`, …). `paused` and `chain_frozen`
  answer `409` instead of `423`.
- Route files name only what differs per route or environment (`route`, `version`, `product`,
  `chain.{chain_id, forwarder_factory, treasury}`, `asset.{symbol, contract, decimals}`,
  `pricing.{primary, check}`, and `limits`); every other value is a code default, overridable
  under its key (architecture §14). `topup route show FILE` prints the resolved route. The
  implementation defaults to the factory's first `CREATE`, the sanctions oracle to Chainalysis's
  address on chains that have one, and a product's key id is `{product}/v1`. `finality`,
  `destination.product_kid`, and `rate_lock.enabled` are removed; the `quotes` pause scope stops
  quote creation. Staging's route moves to version 2.
- A new quote's `amount_atomic` (and its `payment_uri`) is rounded up to the route's
  `quote.amount_decimals` token decimals, default 4, so the payer is asked for `273.9185` PHA
  rather than 18 decimals. The rounding overpays by less than one unit of the last decimal; the
  quote's `amount` credit is unchanged. Quotes created before keep their amount.
- The admin deposit `nudge` and refund `approve`/`record` paths take the `dep_` and `re_` ids the
  product API returns, as the admin deposit view and outbox replay already did; the bare UUID
  still works. Their responses (`AdminRefundResponse.id`, `NudgeResponse.deposit_id`, and the
  deposit view's `id`) show the prefixed id instead of the UUID.
- A malformed path parameter, query string, or JSON body on any route, product or admin, answers
  the `400` error object (`parameter_invalid`, `parameter_missing`, or `parameter_unknown`, with
  `param`) instead of plain text; the admin routes' JSON bodies were plain text before.

- **Breaking: webhook fulfillment replaces the settlement protocol**
  (architecture §7, §11; integration guide §5). A deposit that passes screening is `credited`
  directly (`confirmed → credited`; the `cleared` state is gone), and `deposit.credited` is the
  fulfillment event: the product credits `amount_minor` to `external_id` once per deposit id and
  answers `2xx`. Its payload is now `product_id`, `external_id`, `deposit_id`, `state`, `unit`,
  `amount_minor`, `price_source`, `price_scaled`, `price_scale`, `valuation_at`,
  `product_lock_ref` (the receiving address's lock, also for spot-priced payments), `address`,
  `route`, `route_version`, `chain_id`, `asset_contract`, `tx_hash`, `log_index`, and
  `amount_atomic` (no `destination_tx_id`), and its `webhook-id` is
  `uuid_v5(DEPOSIT_NAMESPACE, "deposit.credited:<deposit_id>")`, the same on every delivery and
  after a restore. Deliveries retry until `2xx`. The service no longer sends
  `POST {settlement_url}` or `GET {settlement_url}/{key}`, and no deposit becomes
  `rejected(product_refused)` any more: a product refuses a credit by holding it and requesting a
  refund. Allowed inside `/v1` without a deprecation window because no product consumed the
  settlement protocol in production (owner decision on #143).

- The attested route's `destination.settlement_url` is removed (routes that set it no longer
  load). A product's `webhook_url` may be `http` only when the service's own public origin is.

- `GET /v1/admin/report/daily` route entries replace `settlements_by_status` with
  `credited_undelivered` and `credited_undelivered_max_age_seconds`: `deposit.credited` events the
  product has not acknowledged yet (administrative API).

- `POST /v1/products/{p}/deposits/{id}/refund-requests` accepts `credited` and `swept` deposits
  too (still not `sanctioned`, still at least the route's `min_refund_atomic`): a product asks to
  refund a credit it did not apply or has reversed, for example for a closed workspace; finance
  approves every request (integration guide §5.4).

- **Settlement conformance:** the `unknown_get` case now requires `404` for `GET` of an unknown
  settlement key and fails `200 {"status":"unknown"}`, which it used to accept. The service
  resends a settlement only after a `404` by key; the other answer made it poll without ever
  resending. `topup-conformance-reference --broken unknown-status` answers the old form and fails
  exactly that case.

- **Breaking (base URL):** the service is served on a custom domain, `https://crypto-topup-api.phala.com`
  (staging `https://crypto-topup-api-staging.phala.com`), with TLS terminated inside the CVM and
  its certificate evidence at `/evidences/`. Sign `@target-uri` for that origin; the gateway URL
  `https://<app_id>-8080.<gateway domain>` no longer answers.

- Removed the unreachable `501` response from `/v1/attestation` and the `work_package` error field
  from `openapi.json`; both belonged only to the pre-C11 placeholder, and production never
  returned them (#90).
- Rate locks carry an optional `payment` object, chosen by the lock consumption rule: the
  deposit that consumed the lock, otherwise the first payment that would consume it, otherwise
  the first payment. `status` is `"seen"` while it is above `finalized` (with `confirmations`
  and `estimated_final_at`, block time plus 15 minutes) and `"finalized"` once it is a deposit
  (`deposit_id` locates it); `supported`, `in_time`, and `amount_within_tolerance` describe it
  against the lock and are false on a cancelled lock.
- New `GET /v1/products/{p}/accounts/{ext}/pending-deposits` lists transfers to the account's
  persistent addresses seen above `finalized`. They are not deposits and are not credited; once
  final they leave this list and appear under `deposits`, and a reorg can remove them.

See `docs/architecture.md` §8 and §12. Both are display only: crediting is unchanged and still
happens only from two-provider finalized data.

- New event `deposit.pending`, sent at most once per chain event when a non-zero transfer of a
  routed token to a watched address is first seen above `finalized`. Its payload is marked
  `provisional: true`; it never changes a balance, and the transfer may still disappear in a
  reorg. It may arrive after `deposit.credited` for the same deposit, so act on fetched state,
  not event order. Receivers that do not handle it must ignore it, as with any unknown event
  type.
- `deposit.credited` and `deposit.rejected` payloads now include `chain_id`, `state`
  (`credited` or `rejected`), and `route` (null when no route was selected); `chain_id` and
  `route` match the fields already on `deposit.confirmed`. This covers every producer, including
  the scanner's `unsupported_asset` rejection. The change is additive; existing fields are
  unchanged. See `docs/architecture.md` §12.

- A lock now expires by chain time: `rate_lock.expired` is emitted only once the finalized chain
  has passed `expires_at` and no payment mined inside the window awaits confirmation, so a payment
  made in the last minutes of the window is consumed at the lock price and never reported as
  expired. Until then `GET …/rate-locks/{ref}` returns `status: open` with
  `remaining_seconds: 0`; expiry events arrive about 15 minutes after `expires_at`. See
  `docs/architecture.md` §9.
- `DELETE …/rate-locks/{ref}` on a lock whose payment window has closed but which has not yet
  expired now answers `409` with the new error code `window_closed` ("payment window has
  closed") instead of `conflict`. `conflict` remains for consumed or expired locks.
- `GET …/limits` `reset_at` is the earliest payment-window close among open reserved locks. It can
  be in the past: exposure is released only at chain finality, about 15 minutes later.

### Removed

- Webhook events written before Stripe-style events (outbox format 1) and their old envelope;
  every event is `{id: "evt_…", object: "event", type, created, data: {object}}`.
- The retired `rejected(product_refused)` reason and `cleared` state, and addresses issued before
  quotes (persistent addresses).
- **Settlement conformance suite** (`topup-conformance`, `topup-conformance-reference`,
  `docs/conformance.md`, `make product-conformance`) and the reference product's conformance mode
  (test accounts and the `_conformance/ledger` hook). The settlement endpoint it tested is being
  replaced by webhook fulfillment (integration guide §5); webhook receivers are
  tested with `topup-sdk send-test-event`.

- **Breaking (administrative API):** `GET /v1/admin/report/daily` route entries no longer carry
  `exposure_minor`, `exposure_minor_reason`, `pnl_minor`, or `pnl_minor_reason` (#94). They were
  always null placeholders; route exposure now comes from the report-level `exposure_minor`, and
  PnL is not defined precisely enough in the design to compute. Allowed as a pre-GA exception:
  the endpoint is admin-only and no service has been deployed.
