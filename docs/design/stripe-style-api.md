# Design: a Stripe-style product API and minimal configuration

Status: proposal, for review before any code change. Owner decision: "精简, 学习 Stripe" — make the
product API and the configuration as lean as Stripe's, following Stripe's documented API
conventions. Nothing consumes the API externally yet
([phala-cloud-monorepo#2196](https://github.com/Phala-Network/phala-cloud-monorepo/pull/2196) is
docs only; staging's only product is the reference product), so `/v1` changes in place and the
deprecation window of [integration.md §9](../integration.md#9-versioning-and-deprecation) does
not apply, as with the move to webhook fulfillment.

What does not change: contracts, CREATE2 math, the deposit state machine, valuation, screening,
flush, reconciliation, attestation, RFC 9421 request signing, Standard Webhooks `v1a` delivery,
and the fulfillment contract (one signed `deposit.credited` per deposit, credited once by its id).

## 1. Stripe conventions applied

Each row was checked against Stripe's API reference on 2026-09-26. Where this service departs
from Stripe, the last column says why.

| Convention (source) | Stripe | Here |
|---|---|---|
| Resource paths ([API reference](https://docs.stripe.com/api)) | Top-level nouns: `/v1/payment_intents`, `/v1/refunds`; actions as `POST /v1/payment_intents/{id}/cancel` ([cancel](https://docs.stripe.com/api/payment_intents/cancel)) | `/v1/quotes`, `/v1/deposits`, `/v1/refunds`, `POST /v1/quotes/{id}/cancel` |
| Caller identity | The API key identifies the account; no account segment in the path | The RFC 9421 `keyid` identifies the product; `/v1/products/{p}` goes away |
| Related objects | Id fields named after the resource (`customer`, `payment_intent`, `charge`) | `quote`, `deposit`; `account_id` is the product's own id for its customer (the role of Checkout's `client_reference_id`), not a service resource, so it keeps `_id` |
| Lists ([pagination](https://docs.stripe.com/api/pagination)) | `{object: "list", url, has_more, data}`, newest first; `limit` 1–100 (default 10), `starting_after` / `ending_before` object ids, mutually exclusive | Same |
| Errors ([errors](https://docs.stripe.com/api/errors)) | `{error: {type, code, message, param, doc_url}}`; `type` ∈ `api_error`, `card_error`, `idempotency_error`, `invalid_request_error`; statuses 200, 400, 401, 402, 403, 404, 409, 424, 429, 5xx | `{error: {type, code, message, param}}`; `type` ∈ `invalid_request_error`, `idempotency_error`, `api_error`; statuses 200, 400, 401, 404, 409, 429, 500, 503 (423 is dropped) |
| Idempotency ([idempotent requests](https://docs.stripe.com/api/idempotent_requests)) | `Idempotency-Key` header on any `POST`, up to 255 characters; same key with different parameters is an error; keys may be pruned after 24 h | Same header, on `POST /v1/quotes` and `POST /v1/refunds`, covered by the request signature. The key is stored on the created object and never pruned, so a retry always returns that object |
| Events ([Event object](https://docs.stripe.com/api/events/object)) | `{id, object: "event", type, created, data: {object}}`; `type` is `resource.action`; `created` is Unix seconds; `data` never changes | Same envelope; delivered with Standard Webhooks headers, not `Stripe-Signature` (asymmetric: the product holds only a public key) |
| Expansion ([expanding](https://docs.stripe.com/api/expanding_objects)) | Id fields marked expandable become objects with `expand[]=field`; lists use `data.field`; depth ≤ 4 | `expand[]` for `quote` on a deposit, `deposit` on a quote and a refund; depth 1 |
| Amounts ([currencies](https://docs.stripe.com/currencies)) | Integer in the currency's minor unit (`1099` = 10.99 USD); lowercase ISO currency | `amount` integer US cents with `currency: "usd"`. Token amounts stay decimal strings (`amount_atomic`): 18-decimal values exceed JSON's safe integer range |
| Timestamps | Unix seconds (`created`) | Same: `created`, `expires_at`, `valued_at` |
| Object ids | Prefixed opaque ids (`pi_…`, `re_…`, `evt_…`) | `qt_`, `dep_`, `re_`, `evt_` + 32 lowercase hex of the UUID. The deposit and credited-event UUIDs stay UUIDv5, so both are still recomputable |
| Test mode | `livemode` flag, test keys | The staging origin is test mode; no flag |
| Authentication | Secret bearer key | RFC 9421 ed25519 signatures (unchanged): the service stores only the product's public key |
| Request bodies | Form-encoded | JSON (unchanged; the signature covers `content-digest`) |

## 2. The product API

### 2.1 Endpoint table

Every product request is signed; the `keyid` names the product (§2.8).

| Method and path | Purpose | Replaces |
|---|---|---|
| `GET /v1/config` | What the integrator's UI reads instead of hardcoding: assets, limits, quote terms, finality time (§2.2) | `GET …/accounts/{ext}/limits` (static part) |
| `POST /v1/quotes` | Create a quote: `{account_id, amount, currency, chain_id, asset}`; `Idempotency-Key` | `POST …/accounts/{ext}/rate-locks`; implicit account creation stays |
| `GET /v1/quotes/{id}` | Resume a checkout; carries the display-only `payment` | `GET …/rate-locks/{ref}` |
| `POST /v1/quotes/{id}/cancel` | Cancel an unpaid quote | `DELETE …/rate-locks/{ref}` |
| `GET /v1/deposits` | List, newest first; filters `account_id`, `quote`, `status`, `tx_hash`, `created[gte]`, `created[lte]` | `GET …/accounts/{ext}/deposits`, `GET …/deposits?tx_hash=\|address=\|lock_ref=` |
| `GET /v1/deposits/{id}` | One deposit | `GET …/deposits/{id}` |
| `POST /v1/refunds` | Request a refund: `{deposit, destination_address, amount_atomic?}`; `Idempotency-Key` | `POST …/deposits/{id}/refund-requests` |
| `GET /v1/refunds/{id}` | One refund | — (new) |
| `GET /v1/attestation?nonce=` | Settlement key evidence; unauthenticated | unchanged |

Removed without replacement in the product API:

| Removed | Why, and where it goes |
|---|---|
| `POST …/accounts` | Accounts are created by their first quote; registration was already optional. |
| `GET\|POST …/accounts/{ext}/deposit-address`, `…/rotate` | Persistent addresses are deleted (§3). |
| `GET …/accounts/{ext}/pending-deposits` | Only listed persistent-address transfers; a quote's `payment` covers quotes. |
| `GET …/accounts/{ext}/limits` (dynamic part) | Static bounds are in `/v1/config`; a quote above the remaining exposure fails with `exposure_cap_exceeded` and a message stating the remaining amount. |
| `POST …/accounts/{ext}/pause\|resume` | An operator action: `POST /v1/admin/products/{slug}/accounts/{account_id}/pause\|resume {scopes, reason}`. |
| Support lookup's `timeline` and `events` | Operator view: `GET /v1/admin/deposits/{id}` returns the deposit with its transitions and webhook deliveries (the role of Stripe's Dashboard). |

The admin API (`/v1/admin/*`) keeps its paths and its admin key; it gains the two rows above and
adopts the error object. `GET /openapi.json` and `/healthz` stay.

### 2.2 Config

```json
{
  "object": "config",
  "currency": "usd",
  "max_open_amount_per_account": 500000,
  "assets": [
    {
      "chain_id": 11155111,
      "asset": "pha",
      "contract": "0x8f40e7e99678f44c88158f049e62817580ab113b",
      "decimals": 18,
      "pricing": "spot",
      "min_amount": 100,
      "max_deposit_atomic": "200000000000000000000000",
      "min_refund_atomic": "20000000000000000000",
      "quote_ttl_seconds": 900,
      "quote_spread_bps": 50,
      "quote_tolerance_bps": 100,
      "typical_finality_seconds": 900
    }
  ]
}
```

- One `assets` entry per loaded route of the calling product (its current version), so a
  per-route override shows up where it applies.
- `min_amount` is the route's minimum credit in cents, for quotes and deposits alike.
  `max_open_amount_per_account` is the per-account open exposure cap, which is summed across
  routes and bounds any single quote. The dynamic remaining amount is not served: quote creation
  refuses with the remaining amount in the message.
- Fee disclosure: quotes are priced at `spot / (1 + quote_spread_bps / 10 000)`; payments valued at
  spot (late, wrong amount, second payment) carry no spread; network fees are the sender's; sweep
  gas is the service's and never reduces a credit (architecture §15). The integration guide
  states this once; the numbers come from here.
- `typical_finality_seconds` is the per-chain code constant already used for
  `estimated_final_at` (15 minutes on Ethereum).
- Derived from the loaded routes of the calling product; one entry per route. The forwarder
  factory and implementation are not here: the product pins them from the attested deployment,
  like the settlement key (§6.1), so the service cannot vouch for its own addresses.

### 2.3 Quote

`POST /v1/quotes` with `Idempotency-Key: <uuid>`:

```json
{"account_id": "team-42", "amount": 2500, "currency": "usd", "chain_id": 11155111, "asset": "pha"}
```

```json
{
  "id": "qt_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
  "object": "quote",
  "account_id": "team-42",
  "amount": 2500,
  "currency": "usd",
  "chain_id": 11155111,
  "asset": "pha",
  "amount_atomic": "100502512562814070352",
  "exchange_rate": "0.24875621",
  "address": "0x…",
  "payment_uri": "ethereum:0x…@11155111/transfer?address=0x…&uint256=100502512562814070352",
  "status": "open",
  "expires_at": 1790410500,
  "created": 1790409600,
  "payment": null,
  "deposit": null
}
```

- `chain_id` and `asset` are required. The first route has one pair, but Phase 3 adds PHA on Base
  (architecture §17): an optional `chain_id` would turn into a breaking requirement then.
- Token-stated quotes (`amount_atomic` in the request) are dropped: Phala Cloud's UI states USD.
  They can come back as an additive parameter.
- `exchange_rate` is the locked price in USD per token as a decimal string with 8 places (today's
  `price_scaled` with scale 8, rendered exactly).
- `status`: `open` → `complete` (a matching payment consumed it) | `expired` | `canceled`,
  following Checkout Session's `open`/`complete`/`expired` and PaymentIntent's `canceled`. The
  database keeps `consumed`/`cancelled`; the API maps them. Chain-time expiry is unchanged
  (architecture §9): the quote stays `open` past `expires_at` until the finalized chain passes it,
  and the UI hides the address once `expires_at` is past.
- `payment` is the existing display-only view, trimmed to what the UI copy needs:
  `{status: "seen" | "final", tx_hash, amount_atomic, confirmations, estimated_final_at,
  matches_quote, deposit}`. `matches_quote` is today's `in_time && amount_within_tolerance`.
- `deposit` is the consuming deposit (expandable), set when `complete`.
- Idempotency: the same key with the same `{account_id, amount, currency, chain_id, asset}`
  returns the stored quote (also while `quotes` is paused, as today); different parameters return
  `409 idempotency_error`. Without a key every call creates a quote. The SDK always sends one.
- Cancel: `canceled` is returned for an open quote and again on a repeat. Refused with `409`:
  `quote_payment_received` (its address received anything), `quote_window_closed` (past
  `expires_at`, awaiting chain-time expiry), `quote_unexpected_state` (complete or expired).
- The address salt is today's lock salt with the quote id as the reference:
  `keccak256(abi.encode(product_slug, account_id, "lock", quote_id))`. The SDK's `lock_salt` and
  the CREATE2 vectors are unchanged.

### 2.4 Deposit

```json
{
  "id": "dep_3f1c2b9e5a7d5e0f9c8b7a6d5e4f3a2b",
  "object": "deposit",
  "account_id": "team-42",
  "quote": "qt_0c6e1d0a9b3f4c2e8d7a6b5c4d3e2f10",
  "status": "credited",
  "rejection_reason": null,
  "chain_id": 11155111,
  "asset": "pha",
  "asset_contract": "0x8f40e7e99678f44c88158f049e62817580ab113b",
  "amount_atomic": "100502512562814070352",
  "amount": 2500,
  "currency": "usd",
  "exchange_rate": "0.24875621",
  "price_source": "quote",
  "valued_at": 1790410320,
  "address": "0x…",
  "from_address": "0x…",
  "tx_hash": "0x…",
  "log_index": 12,
  "block_number": 7012345,
  "amount_refunded_atomic": "0",
  "refunded": false,
  "created": 1790410300
}
```

- `status` is the state machine unchanged: `detected → confirmed → credited → swept`, or
  `rejected` with `rejection_reason`. Refunds do not become a state: a refund does not move custody
  and can be partial, so, like Stripe's Charge, the deposit carries `amount_refunded_atomic` and
  `refunded` (fully refunded).
- `amount` (US cents) and `exchange_rate` are `null` before `confirmed` and on deposits rejected
  before valuation; `asset` is `null` for an unsupported token (`asset_contract` names it).
- `price_source`: `quote` (was `lock`) or `spot`.
- `quote` is `null` only for legacy persistent-address deposits (§3.3).
- The id is `"dep_" + hex(uuid_v5(DEPOSIT_NAMESPACE, "{chain_id}:{tx_hash}:{log_index}"))`: the
  UUID part is today's deposit id.
- `route`, `route_version`, and the valuation `quote` evidence move to the admin view.

### 2.5 Refund

`POST /v1/refunds` with `Idempotency-Key`:
`{"deposit": "dep_…", "destination_address": "0x…", "amount_atomic": "…"}`.
`amount_atomic` defaults to the unrefunded remainder, as Stripe's `amount` does.

```json
{
  "id": "re_7d1e…", "object": "refund", "deposit": "dep_…",
  "amount_atomic": "…", "destination_address": "0x…",
  "status": "pending", "tx_hash": null, "created": 1790500000
}
```

`status` is `pending` while requested, approved, or sent, and `succeeded` once the transfer is
final (Stripe's names; finance's steps stay internal and visible to the admin API). Eligibility is
unchanged (architecture §15); an ineligible deposit is `409 deposit_not_refundable`, an amount
above the remainder `400 amount_too_large` with `param: "amount_atomic"`, a paused `refunds`
scope `409 paused`.

### 2.6 Lists

`GET /v1/deposits?account_id=team-42&limit=20&starting_after=dep_…` returns
`{"object": "list", "url": "/v1/deposits", "has_more": true, "data": [ … ]}`. Order is newest
first by `(created, id)`; the cursor is an object id, so no opaque cursor encoding remains. Other
lists (quotes, refunds) are not added until a consumer needs them; they are additive.

### 2.7 Errors

`{"error": {"type": "invalid_request_error", "code": "amount_too_small", "message": "…",
"param": "amount"}}`. Codes are stable; messages are not.

| Status | `type` | `code` |
|---|---|---|
| 400 | `invalid_request_error` | `parameter_missing`, `parameter_invalid`, `parameter_unknown`, `amount_too_small`, `amount_too_large` (each with `param`) |
| 401 | `invalid_request_error` | `signature_invalid` |
| 404 | `invalid_request_error` | `resource_missing` |
| 409 | `idempotency_error` | `idempotency_key_reused` (same key, different parameters) |
| 409 | `invalid_request_error` | `signature_replayed`, `exposure_cap_exceeded`, `quote_payment_received`, `quote_window_closed`, `quote_unexpected_state`, `deposit_not_refundable`, `paused`, `chain_frozen` |
| 429 | `invalid_request_error` | `rate_limit` (quote creations per account) |
| 503 | `api_error` | `unavailable` (no fresh price, database unavailable) |
| 500 | `api_error` | `internal_error` |

`paused` and `chain_frozen` move from `423` to `409`: 423 is outside Stripe's status set, and
neither should be retried automatically. The SDK retries 429, 5xx, transport errors, and
`signature_replayed`, re-signing, with the same `Idempotency-Key`.

### 2.8 Authentication

Unchanged RFC 9421 profile, with one change: the product is found from the signature's `keyid`,
not the path. A product's key id is `{slug}/v1` (today's `phala-cloud/v1`), derived instead of
attested per route (§4). The verifier reads `keyid` from `Signature-Input`, requires the form
`{slug}/v1`, loads that product's stored public key, verifies, and requires a loaded route to name
the product, as now. Key rotation stays a hard cut under the same key id.

### 2.9 Events

```http
POST {webhook_url}
webhook-id: evt_26a20351ab10595a852f9c1aa0372d73
webhook-timestamp: 1790410330
webhook-signature: v1a,<base64 ed25519 over "{webhook-id}.{webhook-timestamp}.{raw body}">

{"id": "evt_26a20351ab10595a852f9c1aa0372d73", "object": "event", "type": "deposit.credited",
 "created": 1790410321, "data": {"object": { …the Deposit of §2.4… }}}
```

| Event | `data.object` | Id (UUID part) | Today |
|---|---|---|---|
| `deposit.credited` | Deposit | `uuid_v5(NS, "deposit.credited:" + deposit_uuid)` (unchanged) | same name; flat payload |
| `deposit.rejected` | Deposit | `uuid_v5(NS, "deposit.rejected:" + deposit_uuid)` | random id |
| `deposit.refunded` | Deposit (with the new `amount_refunded_atomic`), one per final refund | `uuid_v5(NS, "deposit.refunded:" + refund_uuid)` | random id; refund fields |
| `quote.expired` | Quote | `uuid_v5(NS, "quote.expired:" + quote_uuid)` | `rate_lock.expired` |
| ~~`deposit.pending`~~ | — | — | dropped |
| ~~`deposit.confirmed`~~ | — | — | dropped |

- `data.object` is the API representation at the time of the event, written into the outbox row
  in the same transaction as the state change and never re-rendered, as Stripe's `data` never
  changes.
- Every event id becomes deterministic, so a re-emission after a restore deduplicates for every
  type, not only `deposit.credited`.
- `deposit.pending` and `deposit.confirmed` are dropped: neither may move a balance, the waiting
  screen already polls the quote (whose `payment` shows the seen transfer), and the "Finalizing"
  and "Crediting" UI states come from the fetched deposit. This removes the head scan's event
  writes; the head scan itself stays for `payment`. Stripe has a comparable
  `payment_intent.processing`; it can be added later as `quote.payment_detected` if the product
  asks for the optional "payment received" notification.
- Fulfillment: credit `data.object.amount` cents to `data.object.account_id` once per
  `data.object.id`. The Phala Cloud order key becomes the deposit id itself
  (`provider_order_id = "dep_…"`).

## 3. Persistent addresses are deleted

### 3.1 What goes

Quote first is the only flow, as Stripe's PaymentIntent is: every address belongs to one quote.
Deleted: the three deposit-address endpoints and their models; `persistent_salt` (core, SDK, and
its vectors); `get_or_create_persistent_address`, `rotate_persistent_address`,
`insert_persistent_address`, `find_active_address`; the persistent part of the head scan's
watched set and `addresses.requested_at` with its index; the `addresses` pause scope; the pending
deposits endpoint; `TopupClient.create_deposit_address`, `get_deposit_address`,
`rotate_deposit_address`, `list_pending_deposits`, `register_account`, `lookup_deposits`,
`get_limits`; the persistent parts of the sandbox scenarios, the reference product and its driver,
the SDK example, the integration guide, architecture §1, §4, §8, §12, §15, §18, and the UX rows
"Advanced flow" and the exchange-user copy that recommends the persistent address.

Exchange withdrawals lose their reusable address. The quote page keeps the existing warning that
exchanges may deduct fees, so the received amount must equal the quote; a mismatch is still
credited at spot.

### 3.2 Funds sent to a quote address after its quote ends

Kept exactly as today. The finalized scanner watches every address ever issued on the chain
(`db::scanner::list_scan_addresses` selects all `addresses` rows, retired and lock ones included),
so a transfer to the address of an expired, cancelled, or completed quote becomes a deposit at
finality, is valued at spot, and is credited to that quote's account; its `quote` names the quote
and `price_source` is `spot`. Only the display-only head scan stops watching a quote an hour after
`expires_at` (`db::pending::list_watched_addresses`), so such a late payment shows no `payment`
before finality.

An address the service never issued is watched by nothing: its funds are invisible to the
service and are recoverable only by a separately reviewed Safe action, exactly as today. Products
never derive addresses on their own; they only verify the ones the service returns.

### 3.3 Existing persistent addresses (staging)

Their rows stay, with `kind = 'persistent'`, as history and for custody: the flusher sweeps by the
stored salt and the reconciler checks `addressOf(salt)` against the stored address, neither of
which needs `persistent_salt`. The finalized scanner keeps watching them, so a late payment to one
is credited at spot with `quote: null`, and the admin runbook for workspace closure applies as
before. No new persistent row can be created: the `kind` check keeps the value for history only,
and the one-active-per-account unique index is dropped.

### 3.4 Accounts

Accounts become implicit: created by the first quote, keyed by `(product, account_id)`. No
account endpoint remains. Pausing an account is an operator action (§2.1), because refusing a
customer is the product's policy and it already refuses by holding the credit (integration §5.4).

## 4. Configuration

### 4.1 Principle

A code default is as attested as a route value: the image digest is part of the compose hash, so
design rule 6 ("everything that affects money is measured") still holds when a number moves from
YAML into code. A value stays in the route file only if it genuinely differs between routes or
environments. Every default stays overridable under the same key, and `topup route show
--resolved` prints the effective route (file plus defaults) so a reviewer reads one complete
document, as today.

### 4.2 Route file: every field

The staging route file has 47 leaf values.

| Field | Decision | Default and justification |
|---|---|---|
| `route` | keep | Identity of the route; recorded on every deposit. |
| `version` | keep | Deposits keep the version that created them. |
| `destination.product` | keep, as top-level `product` | Varies per route. |
| `chain.chain_id` | keep | Varies per route. |
| `chain.contracts.forwarder_factory` | keep, as `chain.forwarder_factory` | Varies per deployment. |
| `chain.contracts.treasury` | keep, as `chain.treasury` | The value a finance reviewer checks; startup still verifies it on chain. |
| `chain.contracts.implementation` | default | Read from `factory.implementation()` on both providers at startup; the factory is immutable, and startup already compares the two today. |
| `screening.sanctions_oracle` | default per chain; override where none exists | Chainalysis publishes one address per network: Ethereum and most EVM chains `0x40C57923924B5c5c5455c48D93317139ADDaC8fb`, Base `0x3A91A31cB3dC49b4db9Ce721F50a9D076c8D739B` ([Chainalysis oracle docs](https://go.chainalysis.com/chainalysis-oracle-docs.html)). Sepolia has none, so staging keeps its test oracle as an override. |
| `asset.contract` | keep | Varies per route. |
| `asset.decimals` | keep | Attested, not read from the token, because credit math depends on it. |
| — | add `asset.symbol` (`pha`) | The API's `asset` code (§2.3). One new field. |
| `pricing.primary` | keep | Money-deciding and asset-specific (Coin Metrics asset id). |
| `pricing.check.source`, `pricing.check.symbol` | keep | Asset-specific market. |
| `pricing.check.fx` | default | `kraken USDT/USD`, determined by the check market's quote currency; required only for a non-USDT market. |
| `pricing.mode` | default `spot` | A stablecoin route writes `mode: stablecoin` and omits `check`. |
| `pricing.max_age_s` | default 120 | Coin Metrics publishes the reference rate every minute; two intervals. |
| `pricing.max_deviation_bps` | default 100 | Same value for every route so far; a volatile asset can override. |
| `pricing.max_fx_deviation_bps` | default 50 | USDT/USD is a peg check; 0.5% flags a depeg. |
| `screening.min_credit_minor` | keep, as `limits.min_credit_minor` | Policy, differs by environment. Served as `min_amount`. |
| `screening.max_deposit_atomic` | keep, as `limits.max_deposit_atomic` | Pilot risk bound, differs by environment. |
| `asset.min_refund_atomic` | keep, as `limits.min_refund_atomic` | Finance's dust floor for refunds, per token. |
| `rate_lock.max_open_minor.{account,product,global}` | keep, as `limits.max_open_minor` | Exposure policy, differs by environment; the account cap is served as `max_open_amount_per_account`. |
| `screening.min_deposit_atomic` | default 0 | `min_credit_minor` already rejects small deposits (`below_minimum`) in the unit the user sees; a second token-denominated floor only adds a second rejection reason. |
| `asset.min_flush_atomic` | default 0 | The flusher already requires each address's gas share ≤ `max_gas_ratio_bps` of its value, which is the economic floor; the fixed floor duplicates it. |
| `destination.unit_decimals` | default 2 | Fixed by `currency: "usd"`; the exposure check that all routes agree becomes unnecessary. |
| `destination.product_kid` | derived `{product}/v1` | Today's value; only the stored public key rotates (§2.8). |
| `chain.finality` | removed | `finalized` is the only value startup accepts. |
| `chain.rpc_providers` | default `[provider-a, provider-b]` | Maps to `TOPUP_RPC_PROVIDER_A_URL` / `_B_URL`, the names every compose uses; a second chain overrides with its own ids. |
| `chain.operator_key_version` | default 1 | Set to 2 only after an operator rotation (architecture §15). |
| `chain.flush.native_price_asset` | default from chain | `eth` for chains 1, 11155111, and 8453; any other chain must set it. |
| `chain.flush.schedule` | default `0 */6 * * *` | Same in every route file. |
| `chain.flush.max_gas_ratio_bps` | default 200 | Same everywhere; 2% of value. |
| `chain.flush.max_fee_per_gas_wei` | default 500 gwei | A runaway-fee guard, far above normal Ethereum and Base fees. |
| `chain.flush.replacement_bps` | default 12 500 | A 25% bump satisfies every client's replacement rule (geth needs 10%). |
| `chain.flush.min_operator_balance_wei` | default 0.05 ETH | The mainnet example's value; staging overrides it with today's 0.01 ETH (§9). |
| `rate_lock.enabled` | removed | Quotes are the only flow; stopping them is the `quotes` pause scope. |
| `rate_lock.window_s` | default 900 | Today's value in both route files; about one Ethereum finality delay, long enough to pay from a wallet. |
| `rate_lock.spread_bps` | default 50 | Finance's pilot number; served in `/v1/config`. A different spread for one route is an override. |
| `rate_lock.lock_tolerance_bps` | default 100 | 1% absorbs wallet rounding without accepting a real underpayment. |
| `rate_lock.max_creations_per_minute` | default 10 | Abuse bound per account; no UI needs more. |
| `alerts.stuck_after_s.*` | default 1800 / 1800 / 172 800 | Detected and confirmed normally clear in minutes; credited waits for the next flush (6 h schedule, gas-ratio gated), so two days. |

Result: 21 leaf values for staging (19 on mainnet, which uses the default oracle and the default
gas reserve), down from 47.

```yaml
# deploy/config/routes/phala-cloud-sepolia-pha.yaml
route: phala-cloud-sepolia-pha-usd
version: 2
product: phala-cloud
chain:
  chain_id: 11155111
  forwarder_factory: "0x2407bE5Be2b632F5b166872A49E4946a70CCa531"
  treasury: "0x936c1991f8dA9a919fa11b557a3514719f5A4504"
  sanctions_oracle: "0x28A73f8235d966244210D9c49E34EDdA4fF9e1f6"   # no Chainalysis oracle on Sepolia
  flush:
    min_operator_balance_wei: "10000000000000000"   # 0.01 ETH; the operator holds about 0.02 ETH
asset:
  symbol: pha
  contract: "0x8F40e7E99678F44c88158f049E62817580ab113B"
  decimals: 18
pricing:
  primary: { source: coinmetrics, asset: pha }
  check: { source: binance, symbol: PHAUSDT }
limits:
  min_credit_minor: 100
  max_deposit_atomic: "200000000000000000000000"
  min_refund_atomic: "20000000000000000000"
  max_open_minor: { account: 500000, product: 5000000, global: 10000000 }
```

The route's `version` becomes 2, because the resolved route changes (for example
`min_flush_atomic` 20 000 PHA → 0 and `min_deposit_atomic` 20 PHA → 0). Deposits
keep version 1; the route-retirement runbook applies to version 1 as usual.

### 4.3 GitHub Environment variables

Staging has 17 variables today (plus the repository-level `CI_RUNNER`).

| Variable | Decision | How |
|---|---|---|
| `SENTRY_ENVIRONMENT` | derive | `deploy.yml` already fails unless it equals the Environment name; render `${DEPLOY_ENVIRONMENT}`. |
| `AWS_REGION` | derive | `auto` when `AWS_ENDPOINT` is `https://<account>.r2.cloudflarestorage.com`; required otherwise. |
| `AWS_S3_FORCE_PATH_STYLE` | derive | `true` for the R2 endpoint; required otherwise. |
| `DSTACK_OS_IMAGE` | code constant | Architecture §14 fixes `dstack-0.5.9` and preflight refuses any other; a variable only lets it drift. |
| `TOPUP_ADMIN_KID` | derive | `admin/{environment}-v1` (today's `admin/staging-v1`); an override exists for a future rotation. |
| `PRODUCT_RPC_URL` | derive | Defaults to `TOPUP_RPC_PROVIDER_B_URL`, today the same URL. |
| `TOPUP_GATEWAY_DOMAIN` | derive | `gateway.` + the CVM's `.gateway.base_domain`, which `deploy/phala-cvm.sh gateway` already reads. On `upgrade` it comes from the existing CVM before rendering. On `provision` the node is unknown until the CVM exists, so provisioning renders once, deploys, reads the base domain, and upgrades with the final compose, the two-step the product target already uses for its public URL. |
| `TOPUP_CVM_ID`, `STAGING_PRODUCT_CVM_ID` | keep for now | Could be found by the deterministic CVM name (`crypto-topup-{env}`), which would also remove the `mode` input, but CLI 1.1.22's list output and name uniqueness are not verified; a follow-up. |
| `PHALA_WORKSPACE` | keep | Asserts the API key's workspace; a mis-scoped key would otherwise deploy elsewhere. |
| `AWS_ENDPOINT`, `WALG_S3_PREFIX` | keep | Per environment. |
| `TOPUP_DOMAIN` | keep | Production has no suffix, so it cannot be derived from the environment name. |
| `TOPUP_ADMIN_PUBLIC_KEY` | keep | Per environment. |
| `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | keep | Per chain and environment. |
| `PRODUCT_DRIVER_PUBLIC_KEY` | keep | Staging reference product only. |

Final sets:

| Environment | Variables |
|---|---|
| staging (10) | `PHALA_WORKSPACE`, `TOPUP_CVM_ID`, `STAGING_PRODUCT_CVM_ID`, `TOPUP_DOMAIN`, `TOPUP_ADMIN_PUBLIC_KEY`, `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL`, `AWS_ENDPOINT`, `WALG_S3_PREFIX`, `PRODUCT_DRIVER_PUBLIC_KEY` |
| production (8) | `PHALA_WORKSPACE`, `TOPUP_CVM_ID`, `TOPUP_DOMAIN`, `TOPUP_ADMIN_PUBLIC_KEY`, `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL`, `AWS_ENDPOINT`, `WALG_S3_PREFIX` |

Secrets are unchanged: `PHALA_CLOUD_API_KEY` in GitHub; the storage credentials and Sentry DSN
(and the product seed) sealed in the CVM. Deleting the seven variables is an owner action after
the deploy workflow stops reading them.

## 5. Migration

### 5.1 Schema

One forward migration with a working `down`:

1. `rate_locks` → `quotes`: add `id uuid NOT NULL DEFAULT gen_random_uuid()` (unique; the API's
   `qt_…`), `product_id` (from the address's account), `idempotency_key text` with a unique index
   on `(product_id, idempotency_key)`, and `chain_id`/`asset` filled from the row's route. Existing rows get fresh ids and
   `idempotency_key = addresses.lock_ref`, so their data is kept whole. Their addresses keep their
   stored salts; only new quotes use the quote id as the salt reference. Statuses keep their
   stored names and are mapped in the API.
2. `refunds`: add `idempotency_key text`, unique per product.
3. `addresses`: drop `requested_at` and its index, and the one-active-persistent index; keep
   `kind`, `version`, `retired_at` for legacy rows.
4. Pause scopes: `array_remove(paused_scopes, 'addresses')` on products, accounts, and route
   pauses; tighten the scope checks.
5. `outbox`: add `format smallint NOT NULL DEFAULT 2`, set to 1 for existing rows. Delivery and
   admin replay render format-1 rows in the old envelope, so a replayed old event is still
   byte-identical to its first delivery. The migration refuses to run while any `deposit.credited`
   row is undelivered, so no product receives a mix for one deposit.

### 5.2 Live staging data

- Persistent addresses and their deposits stay, credited history intact (§3.3); their deposits
  show `quote: null`.
- Open rate locks become open quotes and expire by chain time as before; a payment to one is
  consumed at its locked price.
- Deposit and credited-event UUIDs are unchanged; only their rendering gains the prefix. The
  reference product's ledger keys (`deposit:<uuid>`) are migrated to `dep_<hex>` by its own
  schema step in the same release, so a replayed old credit is still recognized.
- Order of rollout: merge the whole API sequence (§8), then deploy topup and the reference
  product from one release. No staging deploy happens between the API PRs.

The staging row counts (persistent addresses, open locks, undelivered events) are read from the
admin daily report before the deploy; this design does not depend on them.

## 6. Impact

### 6.1 SDK (`sdk/python`)

- `topup_client`: regenerated.
- `TopupClient(origin, signer, forwarder=None)`: no product slug (the key id carries it).
  Methods: `get_config`, `create_quote`, `get_quote`, `cancel_quote`, `list_deposits` (an
  auto-paginating iterator, like Stripe's `auto_paging_iter`), `get_deposit`, `create_refund`,
  `get_refund`, `attestation`. Every `POST` gets a generated `Idempotency-Key` reused across
  retries.
- Address check becomes automatic: with `forwarder=(factory, implementation)` pinned from the
  attested deployment, `create_quote` and `get_quote` recompute the address and raise before
  returning a mismatch. Integrators no longer call `lock_salt` themselves.
- Removed: `persistent_salt`, the deposit-address, pending, lookup, limits, and account methods.
- `CreditedDeposit.from_event` reads `data.object`; `credited_event_id` and `deposit_id` return
  prefixed ids; `send-test-event` sends the new envelope.
- Version: 0.1.0 → 0.2.0 (a breaking change while pre-1.0), with a `Removed`/`Changed` changelog
  entry first.

### 6.2 Reference product and sandbox

The reference product's driver and server create quotes instead of addresses, key fulfillment by
`deposit.id`, and read `data.object`. The sandbox scenarios move their persistent payments to
expired-quote payments (spot), and `unsupported_asset` pays another token to a quote address. The
`deposit.pending` assertion in `happy_path` becomes a check of the quote's `payment`.

### 6.3 Phala Cloud PR (#2196)

`docs/phala-cloud-pr/README.md` and the monorepo file change to: fulfill `deposit.credited` by
crediting `data.object.amount` cents to team `data.object.account_id`, order key
`provider_order_id = data.object.id`; store every event by `id`; quote page from
`POST /v1/quotes` with `account_id` = team id and limits and fee copy from `GET /v1/config`;
remove the persistent-address option and the manual address recomputation item (the SDK does it
with the pinned forwarder); refunds with `POST /v1/refunds`; event list `deposit.credited`,
`deposit.rejected`, `deposit.refunded`, `quote.expired`. It stays docs only.

### 6.4 Integration guide rewrite (outline)

Stripe's documentation order, one page, about 400 lines instead of 551:

1. **Quickstart**: generate the key; the operator registers it; pin the settlement key and
   forwarder from attestation; `GET /v1/config`; create a quote; handle `deposit.credited`.
2. **Quotes**: the object, the payment rules (exact and in time → quote price; anything else →
   spot), resume, cancel, the `payment` view, UI copy.
3. **Webhooks and fulfillment**: the envelope, signature verification, the fulfillment
   function and its obligations, holds, event types.
4. **Refunds**: eligibility, destination address from the user, statuses.
5. **Testing**: `topup-sdk send-test-event`, staging on Sepolia, the abnormal payments.
6. **Reference**: authentication, idempotency, errors, pagination, expansion, objects,
   versioning; go-live checklist.

Architecture §9, §12, §14, and §15 are updated to match in the same PRs as the code.

## 7. Size estimate

Estimates from the current files; the implementation PRs report the actual numbers.

| Area | Delete | Add |
|---|---|---|
| Rust `api/` (handlers, repository, models, pending) | ~1 100 (address, account, limits, lookup, pause, pending list, opaque cursors) | ~900 (quotes, config, refunds, lists, expansion, errors, key-id auth) |
| Rust `core` route schema and `topup` route loading | ~250 (removed fields and their validation) | ~150 (defaults, `route show --resolved`) |
| Rust scanner, outbox, events | ~150 (persistent watch, `deposit.pending`/`confirmed` writers) | ~120 (envelope, deterministic ids, format 1) |
| Rust tests (`api.rs`, `rate_locks.rs`, `pending.rs`, others) | ~2 000 | ~1 400 |
| `openapi.json` | 3 261 lines → ~2 000 | |
| `topup_client` (generated) | 10 428 lines → ~6 500 | |
| `topup_sdk`, examples, reference product, scenarios | ~450 | ~300 |
| Docs (integration, architecture, runbooks, Phala Cloud PR) | ~500 | ~350 |
| Route file | 47 → 21 values (staging) | |
| GitHub variables | 17 → 10 (staging), 8 (production) | |

Net: about 4 500 lines of hand-written code and docs removed and 3 300 added, plus about 5 000
generated lines removed.

## 8. Implementation plan

Each row is one PR against `main`. API PRs regenerate `topup_client` and adapt `topup_sdk`, the
reference product, and the scenarios in the same PR (CI fails otherwise), and update the docs
sections they change. No staging deploy until PR 6 is merged.

| PR | Scope | Verification |
|---|---|---|
| 1. Route defaults | Optional fields with code defaults, the flat layout, `implementation` read from the factory, oracle and native-asset defaults per chain, derived product key id, `topup route show --resolved`; route files and example to version 2; architecture §14 | `cargo test`; resolved staging route equals today's values except the listed defaults; `topup route validate` on both files |
| 2. Deploy variables | Derive the seven variables in `deploy.yml` and the render scripts; the provisioning two-step for the gateway domain; deploy README | `deploy/tests/*` updated; an `upgrade` of staging after merge renders a compose identical to today's except the removed inputs |
| 3. API foundations and quotes | Error object, prefixed ids, Unix timestamps, key-id authentication, `Idempotency-Key`, `GET /v1/config`, `/v1/quotes` (create, get, cancel), the quotes migration; old rate-lock and account routes removed | API tests for each error code, idempotent replay and mismatch, cancel refusals, cross-product denial by key id; migration up/down on a seeded database |
| 4. Deposits and refunds | `/v1/deposits` list and get with filters, pagination, and `expand[]`; `/v1/refunds`; admin deposit view and account pause; old deposit, lookup, limits, and refund-request routes removed | Pagination edge cases (both cursors, `has_more`), expansion, refund eligibility and remainder |
| 5. Delete persistent addresses | Endpoints, salts, head-scan watch, `requested_at`, the `addresses` scope, SDK helpers, scenarios; legacy rows kept watched by the finalized scanner | Scanner test: a transfer to a legacy persistent address and to an expired quote's address are both credited at spot |
| 6. Events | Stripe envelope, deterministic ids for every type, `quote.expired`, drop `deposit.pending` and `deposit.confirmed`, outbox `format`, the undelivered-credit guard | Outbox tests: payload snapshot, ids, a format-1 replay is byte-identical; `send-test-event` against the reference product |
| 7. Guides | Integration guide rewrite (§6.4), Phala Cloud PR file, CHANGELOGs | Links and code samples run against the local sandbox |

Then one release: deploy topup and the reference product to staging, run the deposit driver's
abnormal paths, and update #2196.

## 9. Decisions

Approved by the owner with these answers:

1. `deposit.pending` and `deposit.confirmed` are dropped; UI progress comes from
   `GET /v1/quotes/{id}` (PR 6).
2. Ids are prefixed `qt_`/`dep_`/`re_`/`evt_` (PR 3).
3. Quoting by token amount and the dynamic limits endpoint are dropped (PR 3).
4. `min_deposit_atomic` and `min_flush_atomic` default to 0: the gas-ratio rule governs flush
   economics and `min_credit_minor` still rejects dust (PR 1). **Finance confirms these numbers,
   and every other default of §4.2, before production.**
5. Staging overrides `chain.flush.min_operator_balance_wei` to 0.01 ETH: its operator holds about
   0.02 ETH, so the 0.05 ETH default would alert at once (PR 1).
