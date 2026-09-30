# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); the SDKs keep their own changelogs in
`sdk/js` and `sdk/python`.

## [Unreleased]

### Added

- Deposits carry `receipt_log_index` and `revision`, the position and revision their `id` is
  derived from, and `block_hash` and `block_time` (Unix seconds), in the object and every
  `deposit.*` snapshot. A snapshot rendered before this release lacks them, so they are optional
  in the OpenAPI schema.
- Admin: `POST /v1/admin/restore/treasuries/apply` applies again, while frozen after a restore, a
  treasury change that applied after the restore point, from the merchant's delivery of its
  `treasury.updated`: only a delivery the service signed, of the restored pending change becoming
  `active`, whose time-lock ended, and screened again when screening answers (a treasury a
  sanctions list names now is refused). It applies at the event's `created`, audited in the same
  transaction, and its events are not sent again. `POST /v1/admin/restore/treasuries/verify`
  reports such a change as `application_lost`, and the treasury it replaced, received `replaced`,
  as `replacement_lost` when that change is sent in the same request as `active` (both were
  `differs`); both are `matches` once the change is restored. A change on a chain without a current
  route stays pending, as the time-lock leaves it.
- Admin: each result of `POST /v1/admin/restore/events` carries `reversed_deposit` for a
  `deposit.reversed`: `restored`, `recorded`, `address_unknown`, `rescanned` (the rescan recorded
  its position first; also a finding status of `GET /v1/admin/restore`), or `identity_missing`.
- Deposits carry `replaces` and `replaced_by` (`dep_…` or `null`), in the object and every
  `deposit.*` snapshot: a deposit recorded for the transfer that took a reversed deposit's receipt
  position after a reorganization names that deposit, and the reversed one names it (see Fixed).
  Both are `null` when the other deposit is in another account or mode.
- `GET /v1/attestation`'s webhook keys carry `standard_webhooks_public_key`, `public_key` as
  Standard Webhooks' `whpk_` and base64. `report_data` binds `public_key` only: pin the derived
  form only if it encodes the attested key (`verify_attestation_binding` checks it).
- Webhook delivery honors a receiver's `Retry-After` on `429` and `503`, in seconds or as an HTTP
  date: the retry waits at least that long, at most an hour.
- Admin: `POST /v1/admin/deposits/{id}/nudge` answers `400 deposit_unexpected_state` for a deposit
  the pump does not process (anything but `detected` or `confirmed`); it was a silent no-op.
- Admin: `POST /v1/admin/restore/quotes` re-issues a quote given out after the restore point from
  the merchant's record of it (the address must be the one its `qt_` id derives over the current
  treasury), backfilled from the restored cursor so a payment made to it is found. Its terms are
  the merchant's record, kept but never applied: a payment to it is credited at spot unless an
  imported, signed `deposit.credited` carries its credit, and its `expires_at` is the restore's
  detection at the latest. A `client_secret` the service issued for the quote is kept, so the
  payer's page reads it again; `POST /v1/admin/restore/deposit_addresses` takes one too.
- Integration guide §2.3, obligation 6: keep each webhook delivery as your receiver got it, once
  per `webhook-id`, in the transaction that applies it: the raw body bytes and the `webhook-id`,
  `webhook-timestamp`, and `webhook-signature` headers. After a service restore only a delivery
  the service signed is imported (§5.12), so a parsed or re-serialized event cannot prove a credit;
  keep every quote and deposit address response whole too, its `client_secret` stored like a
  credential.
- The reference product (`deploy/product`) keeps a webhook inbox (each verified delivery as
  received, with its processing state, committed with its ledger effect; a redelivery with another
  body keeps the first and is logged) and each quote and deposit address response it gets, client
  secret included, in its ledger, now mode 0600; an existing ledger is migrated in place.
  `python -m reference_product export-restore-records` (from the ledger, read-only) or
  `fetch-restore-records` (from its account API, `GET /accounts/restore-records`, signed with the
  driver key) prints them as the bodies of `POST /v1/admin/restore/treasuries/verify`,
  `/treasuries/apply`, `/deposit_addresses`, `/quotes`, and `/events`; `--since` keeps what was
  created or recorded from five minutes before the restore point on. The controlled restore drill runs the product's receiver and uses
  only what it exports, not events the drill made up; it now also restores a treasury change lost
  with the restore, an unvalued reversed deposit, and a reorganized deposit's revisions.
- Admin: `GET /v1/admin/attestation?account=&livemode=&nonce=` returns `GET /v1/attestation` of any
  account and mode, so the operator verifies a restored instance, where merchant keys are refused.
- Admin: `POST /v1/admin/restore/unfreeze` requires `quotes_reissued`.
- Admin: `POST /v1/admin/restore/delivered_credits/discard` releases a deposit held because its
  transfer contradicts the delivered event imported for it (a `contradicted` finding of
  `GET /v1/admin/restore`); the deposit is then valued from the chain.
- Deposits carry `final_at` (Unix seconds; `null` until `final`), when the finality watch found
  the deposit's block final, in the object and every `deposit.*` snapshot.
- A path or method the API does not serve answers `404 resource_missing` with the error object
  (it had an empty body). The error object's `doc_url` is optional in the OpenAPI schema, as in
  Stripe's; every error of the service still carries it.

- Staging is reset (deploy/README.md, "Staging reset"): its route `phala-cloud-sepolia-pha-usd`
  is version 3 on the deterministic factory `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747`
  (implementation `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9`), with `confirmations: 2`. Update
  the forwarder you pin; accounts, keys, treasuries, endpoints, and webhook keys are created anew.
- Staging adds a second test-mode route, `phala-cloud-sepolia-usdc-usd`: Circle's testnet USDC on
  Sepolia (`0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238`, 6 decimals; <https://faucet.circle.com>),
  valued at one dollar (pricing mode `stablecoin`) with no quote spread, so a $10 quote asks 10 USDC.
  `GET /v1/config` lists it beside PHA, and a deposit address takes both tokens.
- Staging adds Base Sepolia (84532), with the same two test-mode routes:
  `phala-cloud-base-sepolia-pha-usd` (test PHA `0x1a6F260377e42ead1418C7C1afDFD5DE371A9284`, 18
  decimals, public `mint`) and `phala-cloud-base-sepolia-usdc-usd` (Circle's testnet USDC
  `0x036CbD53842c5426634e7929541eC2318f3dCF7e`, 6 decimals, at one dollar with no quote spread), on
  the same factory and implementation. Deposits there are credited at the chain's `safe` head
  (`GET /v1/config` reports `confirmations: "safe"`), typically about 5 minutes after inclusion.

- Launch hardening (docs/design/multi-tenant.md, "launch hardening" amendment):
  - Restricted keys: `POST /v1/api_keys {"type": "restricted", "permissions": [...]}` issues a
    `ppay_rk_{test,live}_` key holding only those permissions (a `write` includes its `read`);
    `api_key` objects gain `permissions`. Restricted keys can no longer be granted `account.write`
    or `endpoints.write`: keys, treasuries, webhook endpoints, webhook keys, and account settings
    need a secret key.
  - `POST /v1/account/webhook_keys/roll`: in live mode `expires_in` is 172800 (48 hours) to 604800;
    `0` is refused there. The default is 172800 (was `0`). The roll's `account.updated` is signed
    by the retiring key too, whenever it is delivered.
  - Treasuries gain `crediting_paused` and `crediting_paused_by`; `POST /v1/treasuries/{id}/pause`
    and `/resume` (secret key), and the admin `POST
    /v1/admin/accounts/{account}/treasuries/{treasury}/pause|resume {reason}`, hold deposits to
    every forwarder over a treasury `pending` without `deposit.credited` until resumed; each change
    is `treasury.updated`.

- Ledger correctness (docs/design/multi-tenant.md, "ledger correctness" amendment):
  - Deposits carry `amount_refunded` (the cents of `amount` succeeded refunds take back, pro rata
    to the refunded tokens, rounded down, cumulative) and `amount_reversed` (`amount` once
    `reversed`), in every `deposit.*` snapshot. Balance rule: a deposit nets to
    `amount - amount_refunded - amount_reversed` while `credited` or `reversed`, 0 otherwise;
    merge snapshots per deposit (the later status and the larger amounts win) whatever the order.
  - A refund with a transaction attached (`mark_paid`) can no longer be canceled
    (`400 refund_unexpected_state`); it is `failed` with the new `failure_reason`s
    `transaction_dropped` (its nonce consumed by another transaction at finality) or
    `transaction_not_found` (never seen within 24 hours), after which a new refund can be
    requested. A deposit's reversal cancels only its refunds without a transaction.
  - `mark_paid` takes `receipt_log_index`, the paying log's position in the transaction's receipt,
    and the refund reports it, replacing the block-wide `log_index`.
  - Per account and mode, the credit of deposits credited but not final is capped
    (`max_unfinalized_credit`, 100 000 cents by default, set by the operator with
    `POST /v1/admin/accounts/{account}`); a deposit past it stays `pending` and is credited once
    final.

- API conformance with Stripe (docs/design/multi-tenant.md, "API conformance" amendment):
  - Business-state failures are `400` (`deposit_not_final`, `deposit_not_refundable`,
    `quote_unexpected_state`, `quote_payment_received`, `quote_window_closed`, `paused`,
    `chain_frozen`, `treasury_not_set`, `treasury_change_pending`, `treasury_unchanged`,
    `treasury_unexpected_state`, `exposure_cap_exceeded`, `deposit_address_cap_exceeded`,
    `webhook_endpoint_cap_exceeded`, `webhook_endpoint_disabled`, `deposit_address_retired`,
    `refund_unexpected_state`, `transfer_already_used`, `api_key_inactive`, `last_api_key`); `409`
    is only `idempotency_key_in_use`. The unused generic `conflict` code is gone. The admin API's
    `signature_replayed` is `401`.
  - A customer's quote-creation and deposit address rotation limits are `429 customer_rate_limit`
    (was `rate_limit`); every `429` carries `Retry-After`.
  - Every response carries `Request-Id: req_…`, replacing `x-request-id`.
  - An `Idempotency-Key` saves the result of every request that started executing, `500`s
    included, and replays it; a request that failed validation (`parameter_*`), was rate limited,
    or met `503 unavailable` is not saved. Validation failures were saved before, `500`s were not.
  - Event `data.object` is rendered in the transaction of the change, not at the first delivery or
    read, and never changes.
  - Treasury events are `treasury.created` (every proven treasury, pending or at once active),
    `treasury.updated` (a pending treasury took effect, or was replaced), and `treasury.canceled`,
    replacing `account.treasury.pending|updated|canceled`; stored events and endpoints'
    `enabled_events` are renamed.
  - A quote's address salt is `keccak256(abi.encode(account, client_reference_id, "quote",
    quote_id))` (was tagged `"lock"`).
  - Admin paths: `POST /v1/admin/reconciliation_blocks/{block_key}/lift` and
    `GET /v1/admin/reports/daily` (were `reconciliation-blocks` and `report/daily`); a route pause
    names its path parameter `{route}`.
  - The OpenAPI document `openapi.json` is the merchant API only; the admin API is
    `openapi.admin.json` (also served at `/openapi.admin.json`). Each object's `object` is a
    single-value enum.

- API vocabulary (docs/design/multi-tenant.md §16 PR 10, and the Stripe-conventions audit): the
  merchant's customer is `client_reference_id` everywhere (`POST /v1/quotes`, quotes, deposits,
  `GET /v1/deposits?client_reference_id=`, and the admin path
  `/v1/admin/accounts/{acct}/customers/{client_reference_id}/pause|resume`).
- Deposit `status` is `pending` (recorded at the route's confirmation, being valued and
  screened, or held by a `settlement` pause), `credited`, `rejected`, or `reversed`; the new
  booleans `final` (its block is final) and `swept` (a finalized `Flushed` event after it moved its
  forwarder's balance) replace the `detected`, `confirmed`, and `swept` statuses. The status filter
  takes the new values.
- A quote's `payment` is the shared `Payment` object: `status` `seen` or `recorded` (was `final`),
  with `chain_id` and `asset`; `matches_quote` is `null` on a deposit address.
- `GET /v1/attestation`'s `quote` is `tdx_quote` (and so is `topup attest`'s output).
- `GET /v1/admin/deposits/{id}` returns the `Deposit` with an `admin` object (`state`, route,
  `transitions`, `events`); the separate admin deposit shapes are gone.
- `livemode` and `metadata` are required on every object in the OpenAPI document.
- `ForwarderFactory` bounds the gas of every call a target makes (`BALANCE_OF_GAS` 30 000 for a
  token's `balanceOf`, `FLUSH_GAS` 200 000 for a forwarder's `flush`), so a token whose
  `balanceOf` reverts, burns gas, or returns short data, or a transfer or treasury hook that burns
  gas, emits `FlushFailed` for its own target only; a `flush` whose gas cannot cover a call's
  whole bound reverts with `InsufficientGas`. The factory and implementation addresses change
  (`deploy/CONTRACTS.md`).

- Events carry `request: {id, idempotency_key}` (the request that caused them; `null` for the
  service's workers), and every `*.updated` event `data.previous_attributes`. New events
  `refund.created`, `refund.updated` (marked paid, canceled, succeeded, failed, metadata), and
  `quote.canceled`.
- Delivery health: webhook endpoints report `pending_deliveries`, `oldest_pending_at`, and
  `last_attempt {at, status_code}`; `GET /v1/events` takes `delivery_success` and `types[]`; the
  admin daily report lists `failing_webhook_endpoints` older than `failing_for_hours` (24).
- Error objects carry `doc_url`, the code's section of the API reference
  (<https://phala-network.github.io/phala-pay/>), built with Redoc from `openapi.json` and
  published from `main`.
- `GET /v1/api_keys` and `GET /v1/treasuries` take `limit`, `starting_after`, and `ending_before`;
  `GET /v1/deposits` takes `created[gt]` and `created[lt]` beside `created[gte]` and
  `created[lte]`, all compared at whole seconds.
- The OpenAPI documents have `servers`, `tags`, and an example of every object and body.
- Restore mode (docs/design/multi-tenant.md §13, architecture §14): after a restore from backup
  the service is frozen until the operator has reconciled it. Every request with an API key
  answers `503 service_restoring` with `Retry-After: 300`, reads included, and no event is
  delivered and no deposit credited meanwhile. Merchants give the operator their records since the restore point
  (integration guide §5.12). Operators: `GET /v1/admin/restore` and
  `POST /v1/admin/restore/{api_keys/revoke, treasuries/verify, webhook_endpoints/delete,
  deposit_addresses, quotes, events, delivered_credits/discard, unfreeze}`
  (`deploy/runbooks/restore.md`); `treasuries/verify`
  re-applies lost treasury cancellations and the merchant's crediting pauses and resumes. On a restore-check
  instance, writes other than these answer `503 service_restoring` (was `503 unavailable`).
- `POST /v1/account {confirmation_policies}` requires, per chain, a confirmation stricter than the
  route's floor (a depth, `safe`, or `finalized`), applied to every deposit not credited yet;
  `GET /v1/config` reports the effective `confirmations` and `typical_credit_seconds`, and the
  account lists its `confirmation_policies`. `POST /v1/account/pause|resume {scopes: ["quotes"]}`
  pauses the merchant's own quotes and deposit addresses; an operator's pause stays until the
  operator lifts it. Both announce `account.updated`.
- `GET /v1/balance` (per chain and token, unswept and final unswept amounts), `GET /v1/sweeps`
  (finalized `Flushed` events as `sw_…` objects), and `GET /v1/forwarders` (`fwd_…`, every issued
  address with its `factory`, `salt`, `treasury`, `quote` or `deposit_address`, and
  `superseded_at`; `sweepable=<token>` lists only forwarders safe to sweep, never one holding a
  sanctioned deposit or paying a sanctioned treasury).
- `GET /v1/quotes` and `GET /v1/refunds` lists.
- Deposit addresses carry `payments` (the last 24 hours, `seen` within about a block, then
  `recorded`) and each create or rotation returns a `client_secret`; with it and no API key,
  `GET /v1/deposit_addresses/{id}?client_secret=` returns the customer's `ClientDepositAddress`.
- `ClientQuote.livemode`, and `payment_status: "reversed"`.

- Treasuries through the API (docs/design/multi-tenant.md D10, §16 PR 7).
  `POST /v1/treasuries/challenge {chain_id, address}` returns an EIP-4361 `treasury_challenge`
  (`message`, `nonce`, `expires_at`; single-use, 10 minutes, or 24 hours for an address that holds
  code); `POST /v1/treasuries {chain_id,
  message, signature}` sets the chain's treasury in the key's mode when the signature is the
  address's EIP-191 signature, or a contract deployed at the address returns `0x1626ba7e` from
  EIP-1271 `isValidSignature` at `finalized` on both providers (ERC-6492 and undeployed contracts
  are refused; the address is screened for sanctions). `GET /v1/treasuries`,
  `GET /v1/treasuries/{id}`, and `POST /v1/treasuries/{id}/cancel`. A `treasury` (`trs_…`) has
  `chain_id`, `address`, `kind` (`eoa` or `contract`), `status` (`pending`, `active`, `replaced`,
  `canceled`), `effective_at`, `replaced_at`, `canceled_at`, and `cancellation_reason`
  (`requested`, or `sanctioned` when a sanctions list named it at its effective time). Current
  treasuries are screened again daily; a listed one pauses the account's `quotes` and
  `settlement`. Safe owners sign the challenge as a Safe message (EIP-712 `SafeMessage`, the
  Safe{Core} SDK's `signMessage`), or approve it with `SignMessageLib`. A chain's first treasury and
  test-mode changes apply at once; a later live change applies after 48 hours unless canceled.
  New events `account.treasury.pending`, `account.treasury.updated`, and
  `account.treasury.canceled` go to every enabled endpoint of the mode whatever its
  `enabled_events`. When a change applies, the chain's network of every deposit address moves to
  a forwarder over the new treasury; the old address stays credited and pays the old treasury.
  New errors: `treasury_proof_invalid`, `treasury_challenge_expired`, `treasury_challenge_used`,
  `treasury_not_deployed`, `treasury_sanctioned`, `treasury_change_pending`,
  `treasury_unchanged`, `treasury_unexpected_state`, and `treasury_not_set`.
- Quotes carry `treasury`, the treasury their address pays.
- Admin `POST /v1/admin/accounts/{account}/pause` and `/resume` `{scopes, reason}`: pause or resume
  scopes of a whole account in both modes, audited and announced as `account.updated`.
- Webhook endpoints managed by the merchant (docs/design/multi-tenant.md D11, §11, §16 PR 8):
  `POST /v1/webhook_endpoints {url, enabled_events, description?, metadata?}`,
  `GET /v1/webhook_endpoints` (cursor pagination), `GET|POST|DELETE /v1/webhook_endpoints/{id}`
  (`disabled: true|false` disables or re-enables), and `POST /v1/webhook_endpoints/{id}/test`
  (a `webhook_endpoint.test` event to that endpoint only). At most 16 per account and mode
  (`409 webhook_endpoint_cap_exceeded`); `url` is `https` on port 443, or in test mode also `http`
  on port 80. The object carries `livemode`, `url`, `enabled_events` (types or `["*"]`), `status`
  (`enabled`, `disabled`), `disabled_reason` (`gone`), `description`, and `metadata`.
- Events API: `GET /v1/events?type&created[gt|gte|lt|lte]` (a type, or a group such as
  `deposit.*`; cursor pagination), `GET /v1/events/{id}`, and
  `POST /v1/events/{id}/resend {webhook_endpoint}` (the same event to one enabled endpoint;
  `409 webhook_endpoint_disabled` otherwise). Events carry `actor` (`key_…`, `admin`, or
  `system`) in the API and in webhook bodies, and `pending_webhooks` in the API: the audit log.
- Account events `webhook_endpoint.created`, `webhook_endpoint.updated` (with
  `data.previous_attributes`), and `webhook_endpoint.deleted`. Account events (`account.*`,
  `api_key.*`, `webhook_endpoint.*`) reach every enabled endpoint of the mode whatever its
  `enabled_events`, and a changed or deleted endpoint receives the event about itself first, at
  its previous URL.
  Permissions `endpoints.read`, `endpoints.write`, and `events.read` are enforced.

- Deposit addresses (docs/design/multi-tenant.md "Deposit addresses"), restored per the owner's
  2026-09-21 requirement, with one address per customer for every supported token on every chain
  (the owner's 2026-09-28 decision, exchange practice). `POST /v1/deposit_addresses
  {client_reference_id}` returns the customer's active `deposit_address` (`da_…`), issuing it the
  first time and adding a network supported since; `GET /v1/deposit_addresses/{id}`,
  `GET /v1/deposit_addresses?client_reference_id&status` (cursor pagination), and
  `POST /v1/deposit_addresses/{id}/rotate`, which retires it and returns the next version, a new
  address on every chain. The object carries `livemode`, `address` (the address shared by every
  network, or `null` when a network's treasury, and so its address, differs), `version`, `salt`
  (`keccak256(abi.encode(account, livemode, client_reference_id, "deposit_address", version))`, no
  chain or asset), `status` (`active` or `retired`), `created`, `retired_at`, `metadata` (set on
  create by merging, updated by `POST /v1/deposit_addresses/{id}`, carried by rotation, and copied
  to each deposit to the address), and `networks`: per chain of the mode, `chain_id`, `address`,
  `treasury`, and `assets` (`asset`, `contract`, `decimals`, and an EIP-681 `payment_uri` without an
  amount). A transfer of any supported token to an active or retired deposit address is credited
  at spot through the quote pipeline, in about 30 seconds; an unsupported token is rejected as at
  a quote's address. Its deposit has `quote: null` and the new field `deposit_address`, and
  `GET /v1/deposits` filters by `deposit_address`. A treasury change on one chain changes only that
  chain's address; the old one stays credited and its refunds are paid from the old treasury. New
  errors: `409 deposit_address_cap_exceeded` (active addresses per account and mode, default
  100 000 live and 1 000 test), `409 deposit_address_retired`, and `429 rate_limit` past 10
  rotations per customer per hour. New addresses are refused with `409 paused` while `quotes` is
  paused, and with `409 chain_frozen` when every chain is frozen; a frozen chain gets no new
  network.
  Permissions `deposit_addresses.read` and `.write` join the authorization table.

- `POST /v1/account/webhook_keys/roll {expires_in}` (docs/design/multi-tenant.md D11, §16 PR 6):
  the next version of the mode's webhook key signs every delivery, and the current one keeps
  signing beside it for up to 7 days (`0` stops it at once), so each delivery carries one `v1a`
  entry per key. `GET /v1/account` lists the versions as `webhook_keys` (`version`, `expires_at`);
  the roll is announced as `account.updated`. Needs `account.write`.
- `livemode` on quotes, deposits, refunds, and `/v1/config`; events carry `account` (`acct_…`) and
  `livemode`.

- `GET /v1/account`; `GET|POST /v1/api_keys`, `GET|DELETE /v1/api_keys/{id}`, and
  `POST /v1/api_keys/{id}/roll {expires_in}` (the old key works for up to 7 days; `0` revokes it).
- Events `api_key.created`, `api_key.updated`, `api_key.revoked`, and `account.updated`; every
  event records its `actor` (an API key id, `admin`, or `system`).
- Stripe-style `metadata` on quotes, deposits, and refunds (docs/design/multi-tenant.md D15,
  [docs.stripe.com/api/metadata](https://docs.stripe.com/api/metadata)): up to 50 string
  key/value pairs, keys of up to 40 characters without square brackets, values of up to 500
  characters. Set it with `metadata` on `POST /v1/quotes` and `POST /v1/refunds`; update it with
  the new `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, and `POST /v1/refunds/{id}`, which
  merge (`""` unsets a key, `metadata: ""` unsets all). Invalid metadata is
  `400 parameter_invalid` with `param` `metadata[key]` or `metadata`. A deposit starts with a
  copy of its quote's metadata, so it arrives in `deposit.credited`'s `data.object`. Objects and
  webhook payloads always carry `metadata` (`{}` when empty); the `client_secret` view does not.
  Secret keys gain `deposits.write`. Do not store sensitive information in metadata.

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

- While the service is frozen after a restore, a merchant request without a well-formed API key
  answers `401 api_key_missing` or `401 api_key_invalid`, as when not frozen (it answered
  `503 service_restoring`): the key's form and checksum are checked first, without a database
  read. A well-formed key still answers `503 service_restoring`, reads included, and nothing is
  saved for its `Idempotency-Key`.
- An `Idempotency-Key` older than 24 hours is pruned in the background, not while a `POST` claims
  its own key, so no request pays for pruning every account's keys; such a key is still free for
  any request.
- **Breaking** (nothing is live): A key rolling itself (`POST /v1/api_keys/{id}/roll` with the
  requesting key's own id) must keep working for at least an hour: `expires_in` under `3600`,
  including the default `0`, is `400 parameter_invalid`. A replay never returns the new key's
  secret, so an immediate self-roll whose response was lost locked the account out; now the old
  key rolls the new one (its id is in the replay) to recover. To stop the old key sooner, revoke
  it with the new key. Another key may still roll a key with `expires_in: 0`.
- **Breaking** (nothing is live): A `client_secret` is `{id}_secret_{nonce}{tag}`, 64 lowercase hex
  digits after `_secret_` (was 48), where `tag` is the service's HMAC of everything before it. A
  forged or malformed secret is refused in memory (`404`) without touching the database or any
  budget, so forgeries can no longer throttle checkout polling; a genuine secret is limited to 120
  reads per minute of its quote or deposit address, with `Retry-After`. Secrets issued before this
  release no longer read anything, on staging included: start the checkout from a new quote, and
  call `POST /v1/deposit_addresses` again for a customer's page (it returns the same active
  address with a new secret).
- **Breaking** (nothing is live): Route files no longer take `unit_decimals`: credit is always USD
  cents, the API's `amount`. A route file that sets it is refused, including a `topup route show`
  output saved before this release, which carries `"unit_decimals": 2`: delete that key.
- **Breaking** (nothing is live): Open-quote caps are per account and mode only (design §12), set by the operator per account
  and mode (defaults: 1 000 open quotes, $50 000 of open quotes per account, $5 000 per customer
  in live mode; 100, $10 000, and $5 000 in test mode). There is no global cap, and test-mode
  quotes never count against live mode. `GET /v1/config` reports the effective caps:
  `max_open_quotes` and `max_open_amount_per_customer` are new, and `max_open_amount_per_account`
  is now the account's cap in the mode (it was the per-customer cap). `400 exposure_cap_exceeded`
  also answers a quote past `max_open_quotes`. Route files no longer take
  `limits.max_open_minor`.
- **Breaking** (nothing is live): Admin: `POST /v1/admin/accounts/{account}` takes `limits {livemode, max_open_quotes,
  max_open_amount_per_account, max_open_amount_per_customer, max_active_deposit_addresses}`, and
  the admin account response carries the effective `limits` of both modes.
- **Breaking** (nothing is live): Deploy runs one production deployment for both modes: every route's `livemode` must match its
  chain (live on a mainnet, test on a test network), and staging takes no live route
  (`deploy/check-route-modes.sh`); production no longer requires every route to be on chain 1.
- **Breaking** (nothing is live): while the service is frozen after a restore from backup, no API
  key authenticates, reads included: every request with a key answers `503 service_restoring`
  with `Retry-After` (only writes did). The restored database can hold a key you revoked after the
  restore point as valid; keys work again once the operator has revoked such keys again and
  unfrozen the service. A quote's or deposit address's `client_secret` read, the admin API, and
  `/healthz` are unaffected. A restore-check instance refuses every merchant key, whether or not
  the freeze is recorded yet.
- **Breaking** (nothing is live): Admin: `POST /v1/admin/restore/events` takes `deliveries`, each
  delivery as the merchant's receiver got it (`webhook_id`, `webhook_timestamp`,
  `webhook_signature`, and the raw `body`), instead of bare event objects, and imports only
  deliveries whose `v1a` signature verifies with the account's webhook keys.

- **Breaking**: quotes and deposit address networks pay the account's treasury of the chain, set
  through the API, instead of the route's: `POST /v1/quotes` is `409 treasury_not_set` on a chain
  without one, and `POST /v1/deposit_addresses` issues networks only on chains with one (`409
  treasury_not_set` when none has). Route files no longer have `chain.treasury` (a route file that
  still names it is refused), and the admin daily report drops `treasury_balance_atomic` and
  `treasury_balance_note`.
- **Breaking**: webhook delivery (§16 PR 8). Retries still continue until delivered (backoff
  capped at 1 h) and a failing endpoint is never disabled (owner decision); `410 Gone` from the
  receiver disables its endpoint at once (`disabled_reason: gone`), announced to the account's
  other endpoints as `webhook_endpoint.updated`. A disabled or deleted endpoint's pending
  deliveries stop; resend them with `POST /v1/events/{id}/resend`. Each endpoint has at most 4
  deliveries in flight, slots go round-robin across endpoints, a failing endpoint is probed one
  delivery at a time after a backoff, and events are not ordered. Deliveries leave through an egress proxy (smokescreen) that refuses
  addresses that are not publicly routable.
- **Breaking**: the operator no longer manages merchants' webhooks: `webhook_url` is removed from
  `POST /v1/admin/accounts` and `POST /v1/admin/accounts/{account}` (an unknown field is `400`),
  and `POST /v1/admin/outbox/{event_id}/replay` and the `topup outbox replay` command are removed.
  Existing endpoints are kept and are now managed through `/v1/webhook_endpoints`.
- **Breaking**: webhooks are signed with a key per account and mode (docs/design/multi-tenant.md
  D11, §16 PR 6), derived in the attested CVM at `settlement/{acct}/{live|test}/v{n}`, instead of
  the one shared `settlement/v1` key: an event signed for one account never verifies at another.
  `GET /v1/attestation?nonce=` now needs an API key (`401` without) and returns `{object:
  "attestation", account, livemode, webhook_keys: [{version, public_key, expires_at}],
  report_data, quote}`, where `report_data = sha256(len(nonce) ‖ nonce ‖ len(account) ‖ account ‖
  livemode ‖ (version ‖ public_key)*)`; `keyid` and `settlement_pubkey` are removed. Pin your
  account's key per mode and check the event's `account` and `livemode`. Test and live events
  are delivered by separate workers.
- **Breaking**: API keys replace RFC 9421 request signing for merchants
  (docs/design/multi-tenant.md D7, D8, D12, §16 PR 5). Send `Authorization: Bearer
  ppay_sk_test_…` or `ppay_sk_live_…`; the key selects the account and the mode. A missing,
  invalid, or revoked key is `401 api_key_missing` or `401 api_key_invalid`, a rolled key past its
  expiry `401 api_key_expired`, and a live key of an account not enabled for live mode
  `403 testmode_charges_only`. Requests are limited per account and mode (100/s live, 25/s test,
  with a test-mode platform ceiling): `429 rate_limit`.
- **Breaking**: every `POST` is idempotent by `Idempotency-Key` for 24 hours per account and mode:
  a repeat of the same request replays the first response (`Idempotent-Replayed: true`), another
  request with the same key is `400 idempotency_key_reused` (was `409`), and a repeat while the
  first runs is `409 idempotency_key_in_use`. A repeated quote creation now returns the same
  `client_secret` instead of a new one.
- **Breaking**: accounts are created only by the operator: `POST /v1/admin/accounts {name,
  contact, due_diligence, charges_enabled, reason, webhook_url?}` returns the first secret keys;
  `POST /v1/admin/accounts/{account}` (was `PUT`) updates live mode, the restricted flag, the
  contact, or the webhook URL, and enabling live mode returns the first live key;
  `POST /v1/admin/accounts/{account}/api_keys {livemode, revoke_existing, reason}` issues a
  recovery key. Customer pauses take `livemode`.

- Every issued address is scanned at every block, not only open quotes' addresses: a late,
  repeated, or wrong-amount payment, or one to a persistent address, is credited at the route's
  confirmation (about 15 seconds after inclusion on Ethereum at depth 2) instead of at finality,
  and shows as `seen` in the quote's `payment` meanwhile. RPC usage no longer grows with polling
  (docs/architecture.md §8): one `eth_blockNumber` per block time and one `eth_getLogs` per new
  block per chain, whatever the number of addresses; the admin-signed `GET /v1/admin/metrics`
  reports the calls per provider, chain, and method (deploy/README.md, "Measuring RPC usage").

- **Breaking:** refunds are paid by the merchant (docs/design/multi-tenant.md D5, PR 9), in
  BTCPay's two-step payout flow. `POST /v1/refunds` creates a `pending` refund of a final deposit
  and reserves its amount; its destination is screened for sanctions
  (`400 destination_sanctioned`, `503 unavailable` when screening cannot answer). Pay it from the
  refund's new `treasury` field (the treasury of the deposit's own address, not the account's
  current one), then attach the transaction with **`POST /v1/refunds/{id}/mark_paid
  {transaction_hash, log_index?}`**. At finality on both providers, a `Transfer` of the deposit's
  token from that treasury to the destination for exactly the amount, in a log no other refund
  holds, makes the refund `succeeded` and sends `deposit.refunded`; anything else makes it `failed`
  with a `failure_reason`, releases the reservation, and sends **`refund.failed`** (Stripe's
  event; event id `uuid_v5(NS, "refund.failed:" + refund UUID)`, `data.object` the refund). **`POST /v1/refunds/{id}/cancel`** cancels
  a pending refund; a reversed deposit cancels its pending refunds. The Refund object gains
  `treasury`, `failure_reason`, and `log_index`, renames `tx_hash` to `transaction_hash`, and its
  `status` is Stripe's `pending`, `succeeded`, `failed`, or `canceled`. New `409` codes:
  `refund_unexpected_state`, `transfer_already_used`. The operator's
  `POST /v1/admin/refunds/{id}/approve` and `/record` are removed, and the daily report's
  `refunds_by_status` counts the new statuses.
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

- The reference product's `team_addresses` table, written with each quote and deposit address but
  never read: the quote and deposit address records hold them. Its ledger (`PRAGMA user_version`
  2) drops the table when the product starts; start it once before a read-only
  `export-restore-records`.
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

### Fixed

- A `client_secret`'s 120 reads per minute of its quote or deposit address refill at two a
  second instead of all at once when a one-minute window rolls over, so a page can no longer read
  120 times just before a rollover and 120 more just after it. A read over the budget is retryable
  after `Retry-After: 1` rather than at the end of the window.
- Admin: `POST /v1/admin/restore/deposit_addresses` refuses, `400`, a `version` more than 32 past
  the customer's latest one, before anything is issued: it issued every version between in one
  transaction, however many, so a huge `version` held its connection and locks until it exhausted
  them. A customer further behind is re-issued in steps (`version` 32, 64, …); an `address` was
  already looked for only that far.
- Admin: `GET /v1/admin/restore` no longer answers `500` for an imported `deposit.reversed` whose
  delivery predates the receipt position (`identity_missing`) and holds a `chain_id` or `revision`
  that is not an integer: the position is read only from integers, and the event is a finding as
  any other.
- Admin: a restore across a treasury change no longer deadlocks the reconciliation. Deposit
  addresses and quotes are re-issued over a treasury in force when they were issued, within 5
  minutes (any since the restore point for a deposit address, around its `created` for a quote),
  not only the current one, once the lost change is applied again with `POST
  /v1/admin/restore/treasuries/apply`; before, they could be re-issued only over the restored
  current treasury, while the change would apply only after the unfreeze those re-issues must
  precede. Each re-issued deposit address version, by address or by version, keeps a superseded
  network over every treasury in force since the restore point, still credited. A restore without
  a restore point re-issues nothing, and a quote the restored database does not hold, created well
  before the restore point, is refused (one it holds is returned, `reissued: false`).
- A quote's `created` is taken once its chain's treasury is read under the treasury lock, and a
  time-locked treasury change records `applied_at` as it applies under that lock rather than when
  the time-lock's pass started, so each falls while the other's treasury is in force.
- Admin: a deposit reversed after the restore point because a re-included transaction put
  another transfer at its receipt position keeps its identity through the restore. Its imported
  `deposit.reversed` rebuilds it, reversed, at its revision, so the rescan records the final
  transfer as its successor again, with the same id, `replaces`, and delivered credit; before, the
  rescan recorded it under the reversed deposit's id, held as `contradicted`, and the successor
  could never be rebuilt.
- Admin: `POST /v1/admin/restore/events` imports the `deposit.reversed` of a deposit that was never
  valued (rejected, such as a token without a route); it refused the whole request with `400`. A
  `deposit.credited` still needs its valuation.
- Idempotent requests are atomic (architecture §12; Brandur Leach's
  [Stripe-like idempotency keys in Postgres](https://brandur.org/idempotency-keys)): every
  merchant `POST` saves its response in the transaction of its changes, so a retry after a crash,
  a dropped connection, or a request slower than a minute replays the result and never runs the
  request twice (a quote, a partial refund, an API key, a webhook endpoint, or a webhook key roll
  was created twice before). A key whose request never saved a response is still taken over by the
  same request after a minute, and the request it replaced can no longer commit: it answers
  `409 idempotency_key_in_use`. A request already making its changes commits, and the repeat
  waits for it (at most 5 seconds, then `409 idempotency_key_in_use`) and replays its response. A
  failure while rendering a response now creates nothing and is replayed as it failed (a quote was
  created before). A request the database cannot begin, or rolls back on a deadlock or
  serialization failure, is an unsaved `503` with `Retry-After`, as is one that finds no database
  connection to claim its key (it was a `500`).
- Admin: `POST /v1/admin/restore/quotes` and `/deposit_addresses` accept a `client_secret` only
  when the service issued it for that id to that account. The secret's nonce now carries an owner
  tag of the account (its length and format are unchanged, and it is still opaque): before, any
  account's record of a lost id with the owner's secret, which the payer's page holds, re-issued
  it under that account and address, and the owner's payer page showed that address. A secret
  issued before this change reads as before but proves no account, so a re-issue refuses it.
- Authorization runs before the idempotency lookup, as Stripe's: a restricted key no longer
  replays a response to a request its permissions refuse, and a `401` or `403` is no longer saved,
  so the same request by a key that holds the permission then runs.

- A payment made through a contract (a router, a swap output) whose transaction is re-included
  before finality against other state, so that the transfer at the same receipt position pays
  another amount or another issued address, is now recorded and credited. The first deposit is
  `reversed` (`deposit.reversed` if you were told of it), and the transfer in the final chain is a
  new deposit with a new id and its own `deposit.credited` (or `deposit.rejected`), already final,
  whose `replaces` names the first one; a quote the first one completed goes to it without
  `quote.expired`. The two deposits' events arrive in no set order: the balance rule nets them
  whatever the order. The new transfer was never recorded, and custody reconciliation froze the
  chain. Existing deposit ids are unchanged, and a transaction re-included unchanged keeps its
  deposit.
- After a restore from backup, a deposit the merchant was told was credited keeps that credit: the
  amount, exchange rate, price source, and valuation time of the imported `deposit.credited` or
  `deposit.reversed` are the deposit's valuation when the rescan re-derives it, instead of a
  re-valuation at spot, so `amount`, `amount_refunded`, and `amount_reversed` match what was
  delivered. A deposit whose transfer on chain contradicts the delivered event is held, not
  credited, until the operator discards the delivered credit.
- A token without a route sent to an issued address is again recorded as
  `rejected(unsupported_asset)`, with its `deposit.rejected` event, once final. Since the per-block
  scanning change, routes in token mode (the default) never saw such transfers: the missing-deposit
  check now reads every issued address's transfers of any token in both modes.
- A webhook endpoint's `pending_deliveries` and `oldest_pending_at`, and the admin daily report's
  `failing_webhook_endpoints`, no longer count the notice of a URL change still pending at the
  endpoint's former URL: it is not a delivery to the endpoint as it is now, so a former URL that
  was taken down no longer makes the endpoint look unhealthy. The notice is still retried until
  delivered.
- Webhook deliveries no longer stall behind a URL change's notice. The notice goes to the
  endpoint's former URL, and its failures there counted as the endpoint's: once that URL was
  taken down, the endpoint cooled down and every probe picked the failing notice first, so new
  events were held for up to an hour at a time. A notice's outcome now neither cools nor clears
  the endpoint.
