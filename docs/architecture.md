# Phala Pay — Design

Status: v7 (Stripe-style product API). Single specification and implementation design. Numbers marked *(policy)* are set
by finance and risk; this document fixes what they mean.

## 0. Standards used

Every mechanism follows a named practice. Where this design adapts a practice, the row says so.

| Mechanism | Standard or reference | Adaptation |
|---|---|---|
| Deposit addresses | CREATE2 forwarders, EIP-1167 via OpenZeppelin `Clones` (BitGo `ForwarderFactory` as reference pattern) | Treasury is an `immutable` on the shared implementation instead of per-clone init |
| Same address on every EVM chain | Arachnid deterministic deployment proxy `0x4e59b44847b379578588920cA78FbF26c0B4956C`, plain CREATE2 salts | Requires identical init code and constructor args per chain |
| Contract roles | OpenZeppelin `AccessControl`; `DEFAULT_ADMIN_ROLE` = finance Safe, `OPERATOR_ROLE` = service key | — |
| Rate-locked deposits | Invoice model (BTCPay Server, Coinbase Commerce): unique address, fixed amount, expiry | Exception rules (§9) are this service's policy profile, not a processor standard |
| Chain reads | JSON-RPC `finalized` tag, `eth_getLogs`, two independent providers | — |
| Price | Coin Metrics Reference Rate (benchmark methodology), checked against the deepest market | — |
| Sanctions | Chainalysis sanctions oracle `isSanctioned(address)` | Direct list screening only; not KYT |
| Deposit identity | UUIDv5 (RFC 9562) over `chain_id:lowercase_tx_hash:decimal_log_index` | — |
| Job queue | PostgreSQL `SELECT … FOR UPDATE SKIP LOCKED` | — |
| Outbound effects | Transactional outbox; at-least-once with idempotent receivers | — |
| Product fulfillment | Stripe Checkout fulfillment: one signed event per paid session, one idempotent fulfillment function | The signature is asymmetric (the product holds only the public key); retries never stop; the event id is derived from the deposit id (§11) |
| Product API shape | Stripe's API conventions: top-level resources, the list object, the error object, prefixed ids, `expand[]`, the Event object, `client_secret` | Requests are signed instead of carrying a secret key; token amounts are decimal strings; §12 lists every departure |
| Idempotent product API | `Idempotency-Key` on quote and refund creation (Stripe; the IETF Idempotency-Key draft), natural keys elsewhere (`(product, account_id)`) | The key is stored on the created object and never pruned; a retry with a fresh signature returns the stored result |
| Request signing | RFC 9421 HTTP Message Signatures, ed25519, `content-digest` | — |
| Webhooks | Standard Webhooks | — |
| Money | Integer minor units; 8-decimal scaled prices | Precision is an application choice |
| Backup | WAL-G base backups plus continuous WAL, `archive_timeout` bounding RPO | — |
| TEE | dstack KMS derivation and attestation verification flow | — |

## 1. Goal

A private service, called by the Phala Cloud billing backend, that turns finalized and
screened deposits of configured tokens into USD credit and tells the product what to credit
with one signed webhook per deposit, which the product fulfills once. Quotes are the only way to
deposit, as Stripe's PaymentIntent is the only way to pay: the user states a USD amount,
receives a locked price, an exact token amount, a single-use address, and a countdown, then
pays. This is the checkout model of Coinbase Commerce and BitPay. A payment that does not match
its quote (late, wrong amount, second payment) is still credited, at the price observed when it
reaches finality. Persistent addresses issued before quotes became the only flow stay watched
by the finalized scanner and their payments are credited at spot, but none is issued again.

Customer contract: *tokens are converted to non-transferable Phala Cloud USD credit at the
published rate observed when the deposit reaches Ethereum finality; the USD value is fixed
after crediting.*

Success: eligible deposits are credited exactly once with no operator step, also after any
outage; balances flush to the treasury automatically; chain, service, and product ledger
reconcile.

First route: Ethereum Mainnet PHA → Phala Cloud USD. New tokens and EVM chains are new route
files; new products are new routes and a registered webhook receiver.

Out of scope: withdrawals, trading, fiat, on-chain credits, bonuses, and everything the
product owns (identity, balance, debt, entitlements, billing policy, welcome promotions).

## 2. Design rules

1. **Addresses have no keys and no service state.** Every address is a CREATE2 forwarder that
   can only pay the treasury, and every salt derives from identifiers the product holds.
2. **Deposits are born final.** The scanner reads only blocks at or below `finalized`. A
   display-only head scan may show a transfer above `finalized` as pending; it never creates,
   rejects, values, or credits anything.
3. **Custody location is a chain fact, not a state.** A flush moves an address's whole
   balance to the treasury at log position `(block, log_index)`; a deposit is flushed iff a
   confirmed `Flushed` event on its address and token is later than the deposit's own log
   position. This is computed from stored events, never stamped from database timing.
4. **One state column, no failure state.** Five progress states plus `rejected`; anything
   else retries forever with capped backoff, and "stuck" is an alert on age.
5. **Price is observed together with finality.** One step records finality and the quote at
   the same instant; there is never a historical price lookup.
6. **Everything that affects money is measured.** Contracts, treasury, thresholds, and spreads
   live in the attested compose. Pause flags are the only runtime-mutable state.
7. **Cross-check every input, and let the product cap the output.** Two RPC providers, two
   price sources; the product may cap credits and verify chain evidence on its own node.

## 3. Trust model

The service runs in a dstack confidential VM. The host and cloud provider cannot read keys or
alter code without changing the attested measurement; products verify by attestation which
code holds the settlement key; the database is inside the boundary.

Deposit addresses are forwarders with an immutable treasury, so **a full compromise of the
service cannot redirect deposited funds**. A credit exists only as a `deposit.credited` event
signed by the attested service's key, which the product pins from attestation. A compromised
service could still sign a credit no deposit backs; as with a card processor, the product
trusts the processor's signed event, and it may bound or check that trust with its own
per-deposit and per-period caps and by verifying the cited log on its own node (§11). Residual
exposure is the operator key's gas balance, kept small.

## 4. Contracts

```solidity
contract Forwarder {                                   // EIP-1167 implementation
    address public immutable treasury;                 // shared by all clones
    address public immutable factory;
    function flush(address token) external onlyFactory; // SafeERC20 full balance → treasury; token == 0 → ETH via call
}
contract ForwarderFactory is AccessControl {           // DEFAULT_ADMIN = finance Safe, OPERATOR = service key
    Forwarder public immutable implementation;         // created in the constructor, so both immutables bind
    function addressOf(bytes32 salt) external view returns (address);   // Clones.predictDeterministicAddress
    function flush(bytes32[] calldata salts, address token) external onlyRole(OPERATOR_ROLE);
        // per salt: cloneDeterministic if no code, then flush(token); emits Flushed(salt, token, amount)
}
```

- `salt = keccak256(abi.encode(product_slug, account_id, "lock", quote_id))`, where
  `quote_id` is the service-assigned `qt_…` id (quotes created before the ids existed used the
  product's lock reference, and legacy persistent addresses `(product_slug, external_id,
  version)`; their stored salts stay authoritative). The product holds every input, so it
  recomputes an address before showing it.
- Each chain configuration records the deployed `forwarder_factory`, its immutable
  `implementation`, and the `treasury`. Address derivation uses the configured factory and
  implementation; startup verifies both against the factory contract before serving traffic.
- Deployed with the deterministic deployment proxy using plain salts, identical init code and
  constructor args on every chain. The treasury is a Safe verified on each chain (deployed,
  same owners and threshold) before a route is enabled.
- Changing the treasury is a new factory and a new route version. The admin Safe only grants
  or revokes `OPERATOR_ROLE`.
- Pilot supports plain ERC-20 with verified behaviour (PHA). Fee-on-transfer or rebasing
  tokens are out of scope; hook-bearing tokens require reentrancy tests before enabling.
- Startup verifies on chain, on every provider: the canonical Multicall3 code hash (balance and
  `addressOf` reads go through it, §14; `topup run` refuses a chain without it), factory code hash,
  `implementation()`, `treasury()`, and `addressOf(sample salt)` against the route file.
- No external audit; internal review + tests. The contracts are two files (~109 lines) built
  from audited OpenZeppelin components (Clones, SafeERC20, AccessControl); funds can only move
  to the immutable treasury; unit, fuzz, and invariant tests cover them; the pilot keeps
  per-deposit and exposure caps. Revisit if caps are raised materially or the contracts change.

## 5. Stack

Rust stable, `#![forbid(unsafe_code)]`, release `overflow-checks = true`. `tokio`, `axum` +
`utoipa`, `sqlx`, `alloy`, `dstack-sdk = "=0.1.3"` for the guest API of dstack 0.5.9, whose
guest agent the deployed OS image `dstack-0.5.9` runs (§14), `secrecy` + `zeroize`. That
crates.io release is the `rust-sdk-v0.5.9` source (commit
`282eeb27d22d8f091ad0fa5a90e638f85cf68751`) with only `hickory-dns` dropped from its `reqwest`
features (commit `f67b4f67ebabef0a27795705121698280a1038dc`); changing the SDK or the OS image
requires a spec change. The dstack 0.6 `/v1` guest API derives different keys for the same domain and no Phala
Cloud node offers a 0.6 image, so it is out of scope until a key migration is specified. `core` denies `arithmetic_side_effects`, `float_arithmetic`, `as_conversions`,
`unwrap_used`. `cargo-deny`, committed lockfile, reproducible distroless image by digest.
Contracts: Solidity with OpenZeppelin, Foundry; no external audit (§4).

```text
crates/core       pure, no I/O: money, route schema, CREATE2 math, state machine, valuation, screening
crates/adapters   chain::evm, signer::dstack, pricing::{coinmetrics,binance,kraken}, risk::oracle
crates/topup      binary: db, pump, scanner, flusher, outbox, reconciler, api, cli
contracts/        Forwarder.sol, ForwarderFactory.sol, deploy scripts, Foundry tests
config/routes     route files (attested)   deploy/  compose + Dockerfile   tests/  integration + contract
```

## 6. Schema

Amounts are `numeric(78,0) CHECK (>= 0)` mapped to `U256`; `transitions` and `audit` are
append-only. Physical addresses belong to an account and a chain; routes are selected per
deposit by `(chain_id, asset_contract)`.

```text
products      id, slug, webhook_url, pubkey, paused_scopes text[]   -- key id: {slug}/v1 (§12)
accounts      id, product_id, external_id, paused_scopes text[]    UNIQUE (product_id, external_id)
              -- external_id is the API's account_id; created by the account's first quote
              -- scopes: quotes | settlement | flush | refunds; empty = active
              -- (`settlement` stops crediting: deposits wait in `confirmed`)
addresses     id, account_id, chain_id, kind (lock | persistent: legacy, never issued again), version,
              lock_ref, salt, address, retired_at
              UNIQUE (chain_id, address)
rate_locks    address_id PK (the quote: qt_ + hex), route, amount_atomic, price_scaled, credit_minor,
              expires_at, status, consumed_by (deposit_id) UNIQUE, product_id, idempotency_key,
              client_secret_hash                  UNIQUE (product_id, idempotency_key)
cursors       chain_id PK, scanned_block, scanned_block_time
pending_transfers  chain_id, tx_hash, log_index, block_number, block_hash, block_time, head_block,
              address_id, asset_contract, from_address, amount_atomic, first_seen_at
              PRIMARY KEY (chain_id, tx_hash, log_index)          -- display only (§8)
deposits      id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
              address_id, account_id, route, route_version, asset_contract, from_address, amount_atomic,
              state, reason, attempt, next_attempt_at, lease_token, lease_until,
              valuation_at, price_scaled, price_source (spot|lock), credit_minor, quote jsonb,
              flush_id, created_at, updated_at
              UNIQUE (chain_id, tx_hash, log_index)
transitions   id, deposit_id, from_state, to_state, attempt, evidence jsonb, created_at
settlements   deposit_id PK, product_id, key, payload jsonb, status, destination_tx_id, receipt jsonb,
              resend_forbidden bool, sent_at     -- read-only history of the retired settlement protocol
flushes       id, chain_id, token, operator, nonce, tx_hash, block_number,
              status (planned|sent|confirmed|reverted), receipt jsonb
              UNIQUE (chain_id, operator, nonce)
flushed       flush_id, address_id, amount_atomic, block_number, log_index   -- one row per Flushed event
              PRIMARY KEY (flush_id, address_id)
refunds       id, deposit_id, amount_atomic, to_address, tx_hash, status (requested|approved|sent|confirmed),
              requested_by, approved_by, idempotency_key, created_at   -- executed from the treasury Safe
outbox        id, event_type, format, product_id, object_type (deposit|quote), object_id,
              payload jsonb, next_attempt_at, delivered_at, response jsonb
              -- format 2: payload is the event's data, rendered at the first attempt;
              -- format 1: rows written before Stripe-style events, delivered unchanged
audit         id, actor, action, subject, reason, created_at
```

Any ERC-20 transfer to one of our addresses becomes a deposit row. The route is chosen by
`(chain_id, asset_contract)`; no route → `rejected(unsupported_asset)`.

Credit: `exp = asset_decimals + price_scale − unit_decimals`,
`credit_minor = floor(amount_atomic × price_scaled / 10^exp)` (multiply when `exp < 0`),
512-bit intermediate, checked into `u64` or `rejected(out_of_range)`. Property tests:
monotone; splitting into `n` parts loses at most `n − 1` minor units.

## 7. States and pump

```text
detected → confirmed → credited → swept
        ↘ rejected(reason)
```

`core::next(state, outcome)` is the only function that picks a target. A step that cannot
finish leaves the state, records the attempt in `transitions`, and retries with exponential
backoff and jitter, 30 s → 1 h, forever; an alert fires past the per-state age *(policy)*.
`rejected` is terminal for credit; its funds are flushed to the treasury like any other and
handled there by finance.

Transitions are applied with `UPDATE … WHERE id = $1 AND state = $expected AND lease_token =
$token`, writing transition and outbox rows in the same transaction. `N` pumps claim with
`FOR UPDATE SKIP LOCKED`, hold a 5-minute lease, run one step with shorter timeouts, persist
once. A step panic aborts the process; the lease expires and another pump re-claims the deposit.

| Step | Does |
|---|---|
| `detected → confirmed` | From both providers: `finalized ≥ block_number`, same block hash, same log. While `detected`, evidence is provisional: if both providers agree on different canonical evidence for the same event identity, the row is corrected. If both are final past the row and neither has the log, the step retries with `log_absent_at_finality`, never a rejection. In the same step, fetch the quote (§8) and store `valuation_at`, `price_scaled`, `credit_minor`, `quote`. Below `min_credit_minor` → `rejected(below_minimum)`. |
| `confirmed → credited` | `isSanctioned(from)` on both providers at a recorded block; `min ≤ amount ≤ max` *(policy)*; account, product, and route not paused for `settlement` (paused → `Wait`, never a rejection). On a pass, the same transaction writes the `deposit.credited` outbox row (§11): the credit is final and owed to the product, whatever the product answers. |
| `credited → swept` | `flush_id` is set: a confirmed `flushed` row exists for the deposit's address and token at a log position `(block_number, log_index)` greater than the deposit's. Evaluated on flush confirmation and on every deposit insert, so backfilled deposits resolve too. |

## 8. Chain, valuation, screening

**Scanner** per chain: read `finalized` from provider A; fetch `Transfer(*, our addresses)`
from any contract in windows ≤ 2 000 blocks and ≤ 1 000 addresses; insert with
`ON CONFLICT DO NOTHING`; advance the cursor after commit. New addresses backfill from
creation (the chain's committed cursor when the address is issued); retired and lock addresses
stay in the filter. Native ETH is a balance check at flush time. Every chain uses the
`finalized` tag; a chain whose finality does not map onto it is enabled only after a reviewed
code change. Later option: Helios as one provider.

**Head scan (display only)** per chain, on provider A, every 12 s (or the scanner poll interval
if shorter): read non-zero `Transfer` logs emitted by the chain's routed token contracts to
watched addresses in `[finalized + 1, latest]` and, in one transaction, upsert the rows seen into
`pending_transfers` and delete rows in that range not seen this time (reorged, or no longer
watched). Other tokens are never requested, so they cannot create pending rows or
notifications; they appear only after finality, as `rejected(unsupported_asset)`. Block times are
read by block hash. The head scan reads the finalized cursor `FOR SHARE`, and the finalized
scanner deletes rows at or below its cursor in the transaction that advances it, so a transfer
moves from pending to deposit atomically and no row below the cursor is written afterwards.
Pending rows never feed deposits, transitions, locks, exposure, credits, or reconciliation;
lock amount and timeliness are computed when read, never stored. When the head scan sees
`finalized` advance it wakes the finalized scanner, and a confirm step waiting for provider B's
finality retries after 12 s instead of the regular wait interval. While reconciliation has frozen
a chain its head scan stops too, so the pending view stops updating.

Watched addresses: quote addresses whose quote is neither completed nor canceled, until one hour
after `expires_at`. Open quotes are bounded by the exposure caps (each reserves at least
`min_credit_minor` against the global cap) and, for the hour after expiry, by the per-account
creation rate limit; any number is requested in batches of 1 000. A payment to any other issued
address (a closed quote's, or a legacy persistent one) shows no `payment` before finality; the
finalized scanner still records it.

**Valuation** happens inside the confirm step, so `valuation_at` is the finality observation
and the price is always current at fetch time. Every route's pricing configuration declares
`mode: spot | stablecoin`; the service never infers the mode from an asset symbol. Spot: primary
Coin Metrics `ReferenceRateUSD` (1-minute), check Binance `PHAUSDT` × Kraken `USDT/USD`; each
observation aged ≤ `max_age` *(policy)* at fetch; `|primary − check| / primary ≤
max_deviation_bps / 10 000`; FX within `max_fx_deviation_bps`; the primary is used. Any failure
retries the whole step. Stablecoin routes use fixed `1.0` with the primary reference rate as a
depeg guard; check and FX observations are not required for that mode.

**Screening** is direct sanctions-list screening plus per-deposit bounds. KYT is a separate
adapter that compliance may require before GA.

## 9. Quotes

Invoice model, enabled from the pilot, with this service's exception profile:

- `POST /v1/quotes {account_id, amount, currency, chain_id, asset}` returns the quote `{id,
  amount, amount_atomic, exchange_rate, address, payment_uri, status, expires_at, …}`.
  `price_lock = price_spot / (1 + spread)` with `spread = spread_bps / 10 000` *(policy)*; the
  user states USD cents and the token amount is rounded up, then up again to
  `quote.amount_decimals` token decimals so the payer reads and types a short amount (the
  overpayment, below one unit of the last decimal, is the payer's; the credit is unchanged).
  `expires_at = now + window` *(policy)*. Quotes count against open-exposure caps per account,
  per product, and global *(policy)*, reserved atomically at creation; creation is rate-limited
  per account. Repeating an `Idempotency-Key` with the same parameters returns the stored quote
  (other parameters are `409 idempotency_error`), also while `quotes` is paused.
- The lock is consumed by the first deposit to its address whose `block_time ≤ expires_at`,
  `asset` matches, and `|amount − locked| ≤ lock_tolerance_bps` *(policy)*; consumption is a
  single `UPDATE … WHERE consumed_by IS NULL`. That deposit is valued at `price_lock` and the
  product receives exactly the `credit_minor` it showed the user.
- Any other deposit to a lock address (late, wrong amount, second payment) is valued at spot
  and still credited; the product shows this rule before payment.
- Expiry uses chain time, like eligibility. A lock expires unconsumed, releasing its exposure
  and emitting `quote.expired`, only once the chain's scanner has committed through a
  finalized block whose time is past `expires_at` (the finalized head's time, read with the
  head, is stored with the cursor) and no deposit mined inside the window still awaits its
  confirm step. A payment mined inside the window is therefore consumed at the lock price and
  never reported as expired. Exposure stays reserved until finality, about 15 minutes after
  `expires_at`, and longer while the scanner is stalled (§16). Until then the API
  shows the quote `open` past `expires_at`, and cancellation is refused once the window has
  closed (`409 quote_window_closed`). A quote whose address has received any payment, even a
  rejected one, can no longer be cancelled (`409 quote_payment_received`).
- Exposure counters sum `credit_minor` across routes, so every route must use the same
  `unit_decimals`; the service refuses to load routes that differ.
- A "quote, then pay to a reusable address" variant is deliberately not offered: matching a
  quote by amount alone is ambiguous, and the single-use address is the processor-standard
  answer.

## 10. Signing and flush

```rust
pub trait Signer {
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx>;   // OPERATOR key, pays gas
    async fn sign_settlement(&self, payload: &[u8]) -> Result<Signature>;
}
```

`signer::dstack` derives `operator/v{n}` (secp256k1) and `settlement/v1` (ed25519) on demand
and zeroizes them (dstack 0.5 derives a key from its domain alone; each domain has one algorithm); `n` is the route's attested `chain.operator_key_version` (≥ 1, initially 1),
and a chain's flusher plans and sends only while that operator holds `OPERATOR_ROLE` on the
factory. `GET /v1/attestation` reports and attests each chain's operator address (§14).

**Flusher** on a schedule *(policy)*, per (chain, token): select addresses whose on-chain
balance ≥ `min_flush_atomic` and whose share of batch gas ≤ `max_gas_ratio` of value
*(policy)*; write `flushes(planned)`; send one `factory.flush(salts[], token)` under the
operator nonce lock; replace with a higher fee on the same nonce if needed; confirm at
`finalized`; write one `flushed` row per `Flushed` event in the receipt with its block number
and log index. Deposits are then linked by the rule in §7, whatever their state. A plan whose
route, product, or account has `flush` paused (§15) when it reaches the front of the operator's
queue is voided unsigned; later plans move down onto its nonce, and its addresses are planned again
once the pause lifts, so one pause never stalls the chain. Recovery after a
crash is by `(operator, nonce)`: if consumed, locate the transaction and read its receipt;
if reverted, mark the flush `reverted` and plan a new one with a fresh nonce; otherwise
rebroadcast the same signed transaction. Every signed transaction (first send, rebroadcast,
replacement) goes to all of the chain's configured RPC providers and counts as sent when any
accepts it with its signed hash, so one rate-limited provider cannot strand a flush; reads use
provider A. Gas is a service cost and never reduces credit.

## 11. Fulfillment webhook

The service tells the product what to credit with one signed event per deposit, and the product
fulfills it once: the pattern of Stripe Checkout fulfillment. The deposit's state never depends
on the product's answer; delivery is tracked on the outbox row.

```http
POST {webhook_url}
webhook-id: evt_<hex of uuid_v5(DEPOSIT_NAMESPACE, "deposit.credited:" + deposit UUID)>
webhook-timestamp: <Unix seconds of this attempt>
webhook-signature: v1a,<base64 ed25519 by settlement/v1 over "{id}.{timestamp}.{raw body}">

{ "id": "<webhook-id>", "object": "event", "type": "deposit.credited", "created": 1790409590,
  "data": { "object": { "id": "dep_…", "object": "deposit", "account_id": "<account>",
                        "quote": "qt_…", "status": "credited", "amount": 1234,
                        "currency": "usd", "price_source": "quote", … } } }
```

- The screen step writes the event in the transaction that moves the deposit `confirmed →
  credited` (§7). The outbox row names the product and the deposit; `data.object`, the deposit
  as `GET /v1/deposits/{id}` returns it, is rendered on the first delivery attempt and stored,
  so retries and replays send the same body. `amount` is the quoted credit when `price_source`
  is `quote`, otherwise the spot credit at finality (§9). `quote` is the receiving address's
  quote, also when a late or wrong-amount payment was valued at spot.
- Delivery is the outbox (§12): at least once, in no order, `2xx` acknowledges, anything else or
  no answer within 20 s is retried with full-jitter backoff (ceiling 30 s doubling to 1 h),
  forever; an undelivered event raises the outbox age warning after 24 hours and the operator
  can replay it. The daily report counts undelivered `deposit.credited` per route.
- The event id is derived from the deposit id, so every retry, replay, and re-emission after a
  restore carries the same `webhook-id`, and the outbox stores one row per deposit. Rows written
  before prefixed ids (outbox `format` 1) keep their flat payload and old envelope
  (`event_id`, `created_at`, `data`) and bare-UUID `webhook-id`, so their replay is byte-identical.

Product obligations:

1. Verify the Standard Webhooks `v1a` signature over the raw body against the pinned
   `(keyid, public key)`, with a timestamp tolerance of 300 seconds.
2. Credit `amount` to `account_id` at most once per deposit id (`dep_…`): the credit
   and its record in one transaction under a unique index, committed before answering `2xx`.
   A repeat is acknowledged without a second credit. A repeat with a different amount can
   only follow a service restore that re-priced a spot deposit (§14); keep the first credit and
   report it.
3. Refuse by holding, never by failing the delivery: a credit for an unknown or closed
   workspace, a suspended account, or above the product's own caps is recorded as held, answered
   `2xx`, and returned through a refund request (§15).

Optional hardening, each the product's choice: fetch `GET /v1/deposits/{id}` and require
`credited` or `swept` with the same amount; recompute the deposit UUID `uuid_v5(NS,
"{chain_id}:{tx_hash}:{log_index}")` and verify the cited log on its own node at finality;
per-deposit and per-period caps as review holds. None is needed for correctness: the credit is
authorized by the service's signature alone.

Phala Cloud: find-or-create an `Order` (`provider = crypto_topup`, `order_flow_code =
'crypto-top-up'`, `provider_order_id` = the deposit id `dep_…`, partial unique index on
`(team_id, provider_order_id)` for that flow), the credit transaction tagged `funding_source =
crypto:<asset>:<chain>`, and `complete_order_payment`, in one transaction. Non-card top-ups skip
the welcome promotion by existing rule.

## 12. API and events

The product API follows Stripe's documented conventions, so an integrator who knows Stripe
knows it. Where it departs, the last column says why.

| Convention | Stripe | Here |
|---|---|---|
| Resources | Top-level nouns, actions as `POST …/{id}/cancel` ([API reference](https://docs.stripe.com/api)) | `/v1/quotes`, `/v1/deposits`, `/v1/refunds`, `POST /v1/quotes/{id}/cancel` |
| Caller | The secret key identifies the account | The RFC 9421 `keyid`, `{product}/v1`, identifies the product; the service stores only its public key |
| Customer reference | Checkout's `client_reference_id` | `account_id`, the product's own id for its customer (a workspace); an account is created by its first quote |
| Ids | Prefixed opaque ids | `qt_`, `dep_`, `re_`, `evt_` and the 32 hex digits of a UUID; the deposit and `deposit.credited` UUIDs are UUIDv5, so both stay recomputable (§0, §11) |
| Amounts | Integer minor units, lowercase currency ([currencies](https://docs.stripe.com/currencies)) | `amount` in US cents with `currency: "usd"`; token amounts are decimal strings (`amount_atomic`), since 18-decimal values exceed JSON's safe integers |
| Timestamps | Unix seconds | Same: `created`, `expires_at`, `valued_at` |
| Lists ([pagination](https://docs.stripe.com/api/pagination)) | `{object: "list", url, has_more, data}`, newest first; `limit` 1–100, `starting_after` or `ending_before` | Same |
| Expansion ([expanding](https://docs.stripe.com/api/expanding_objects)) | `expand[]`, depth ≤ 4 | `expand[]` for a deposit's `quote`, a quote's `deposit`, and a refund's `deposit`; depth 1 |
| Errors ([errors](https://docs.stripe.com/api/errors)) | `{error: {type, code, message, param, doc_url}}` | `{error: {type, code, message, param}}`; `type` is `invalid_request_error`, `idempotency_error`, or `api_error` |
| Idempotency ([idempotent requests](https://docs.stripe.com/api/idempotent_requests)) | `Idempotency-Key` on `POST`, pruned after 24 h | On `POST /v1/quotes` and `POST /v1/refunds`, covered by the signature, stored on the object and never pruned |
| Browser reads | A PaymentIntent's [`client_secret`](https://docs.stripe.com/api/payment_intents/object#payment_intent_object-client_secret) with a publishable key | A quote's `client_secret` alone, for a public subset (below) |
| Events ([Event object](https://docs.stripe.com/api/events/object)) | `{id, object: "event", type, created, data: {object}}`, `Stripe-Signature` | Same body; Standard Webhooks `v1a` signatures, asymmetric, so the product holds only a public key |
| Test mode | `livemode` and test keys | Each environment is its own origin and key; no flag |

Every product request is signed (§3); the product is the signature's `keyid`, which must have the
form `{slug}/v1`, name a registered product with its stored key, and be named by a loaded route.
The verifier rebuilds `@target-uri` from the configured public origin (`TOPUP_PUBLIC_ORIGIN`,
§14) and the request's path and query, never from `Host` or `X-Forwarded-*`, so signers sign the
public URL they call. Signatures are single-use within the acceptance window. Each deployment
(sandbox, staging, production) must pin a distinct product key: single use is recorded per
database, so a shared key would let a signed request be replayed within the freshness window
against another deployment that shares its public origin (for example a replacement or restored
instance). A request for another product's object answers `404`. The admin key can only issue
products and replace their key and webhook URL, pause and resume, nudge, drive the refund
workflow, lift reconciliation blocks (§13), and replay webhook events; each change writes
`audit`.

```text
GET    /v1/config                                                 assets, limits, quote terms
POST   /v1/quotes {account_id, amount, currency, chain_id, asset} single-use address + locked price; Idempotency-Key
GET    /v1/quotes/{id}                                            resume a checkout; unsigned with ?client_secret=: the payer's view
POST   /v1/quotes/{id}/cancel                                     cancel an unpaid quote; later payments credit at spot
GET    /v1/deposits?account_id&quote&status&tx_hash&created[gte|lte]&limit&starting_after&ending_before
GET    /v1/deposits/{id}                                          expand[]=quote
POST   /v1/refunds {deposit, destination_address, amount_atomic?}  rejected, or credited on the product's request; finance approves (§15)
GET    /v1/refunds/{id}
GET    /v1/attestation?nonce=…                                    settlement key and flusher operators (§14)

POST   /v1/admin/products {slug, public_key, webhook_url}   same values → same product, different → 409
PUT    /v1/admin/products/{slug} {public_key, webhook_url, reason}   replace both (§15 Rotation); same values → no change
GET    /v1/admin/deposits/{id}            stored facts, transitions, and webhook events (support)
POST   /v1/admin/products/{slug}/accounts/{account_id}/pause | resume {scopes, reason}
POST   /v1/admin/routes/{r}/pause | resume {scopes}
POST   /v1/admin/deposits/{id}/nudge          next_attempt_at = now; no state change; audited
POST   /v1/admin/refunds/{id}/approve | record {tx_hash}
POST   /v1/admin/reconciliation-blocks/{block_key}/lift {reason}   manual lift (§13); repeat → same lift
POST   /v1/admin/outbox/{event_id}/replay {reason}   redeliver an existing event unchanged
GET    /v1/admin/report/daily                 treasury, unflushed, open quotes, rejected holds, undelivered credits, global exposure, reconciliation blocks
```

Admin paths take an object's prefixed id or, for ids handed out before prefixed ids, its bare
UUID; admin responses show prefixed ids.

**Config.** One `assets` entry per loaded route of the calling product (its current version):
chain, asset code, contract, decimals, pricing mode, `min_amount` (the route's minimum credit in
cents), `max_deposit_atomic`, `min_refund_atomic`, the quote window, spread, and tolerance, and the
typical finality time; plus `max_open_amount_per_account`, the per-account open exposure cap,
which also bounds any single quote. The remaining exposure is not served: a quote above it fails
with `409 exposure_cap_exceeded`, whose message states the remaining amount. The forwarder factory
and implementation are not served: the product pins them from the attested deployment, like the
settlement key, because the service cannot vouch for its own addresses.

**Quote.** `{id, object: "quote", account_id, amount, currency, chain_id, asset, amount_atomic,
exchange_rate, address, payment_uri, status, expires_at, created, payment, deposit,
client_secret}`. `exchange_rate` is the locked price in USD per token, exactly, with 8 decimal
places. `status` is `open`, `complete` (a matching payment consumed it), `expired`, or `canceled`
(Checkout Session's and PaymentIntent's names; the database keeps `consumed` and `cancelled`).
`chain_id` and `asset` are required, so a second route for the same asset is not a breaking change.
Cancel returns `canceled`, also on a repeat, and refuses with `409 quote_payment_received`,
`quote_window_closed`, or `quote_unexpected_state` (complete or expired).

`payment` (display only, §8) is the payment the page should show, chosen by the §9 consumption
rule: the deposit that consumed the quote; otherwise the first payment that would consume it,
finalized deposits before transfers seen above `finalized`; otherwise the first payment at all. It
carries `status` (`seen` above `finalized`, `final` once it is a deposit), `tx_hash`,
`amount_atomic`, and, while `seen`, `confirmations` and `estimated_final_at` (block time plus
15 minutes, the typical Ethereum delay to `finalized`; an estimate); `matches_quote` (right asset,
in time, and within tolerance: it will be credited at the quoted price); and `deposit`, the
`dep_` id it has or will have. On a canceled quote no payment matches. A seen transfer can
disappear in a reorg; only deposits and `deposit.credited` reflect credit. The view ignores pause
scopes, and while a chain is frozen (§13) it stops updating.

**Client secret.** `POST /v1/quotes` returns `client_secret`, `{quote id}_secret_{48 random hex
digits}`, for the payer's checkout page. Only its SHA-256 is stored, so no other response returns
it; a repeat with the same `Idempotency-Key` returns a new secret, and the earlier one stops
working (the product repeats only when it lost the response). `GET /v1/quotes/{id}?client_secret=…`
without signature headers returns the public subset `ClientQuote`: `{id, object, status, amount,
currency, asset, decimals, chain_id, amount_atomic, address, payment_uri, expires_at,
payment_status, confirmations}`, where `payment_status` is `none`, `seen`, `confirming` (final,
being valued and screened), `credited`, or `rejected` (the reason is not exposed). No account,
price, deposit id, or transaction hash. Every unsigned response, errors included, allows any
origin (`Access-Control-Allow-Origin: *`); the secret is the bearer. A secret that is not the
quote's is `404`. Unsigned reads are limited in the process to 120 per quote and 6 000 in total per
minute (`429 rate_limit`).

**Deposit.** `{id, object: "deposit", account_id, quote, status, rejection_reason, chain_id, asset,
asset_contract, amount_atomic, amount, currency, exchange_rate, price_source, valued_at, address,
from_address, tx_hash, log_index, block_number, amount_refunded_atomic, refunded, created}`.
`status` is the state machine (§7); a refund is not a state, because it neither moves custody nor
has to be whole: like Stripe's Charge, the deposit carries `amount_refunded_atomic` and
`refunded`. `amount` and `exchange_rate` are set once valued; `price_source` is `quote` or `spot`;
`asset` is `null` for a token without a route; `quote` is `null` only for a legacy persistent
address. Routes, versions, and valuation evidence are in the admin view.

**Refund.** `{id, object: "refund", deposit, amount_atomic, destination_address, status, tx_hash,
created}`. `amount_atomic` defaults to the unrefunded remainder. `status` is `pending` while
requested, approved, or sent, and `succeeded` once the transfer is final; finance's steps are
visible in the admin API. An ineligible deposit is `409 deposit_not_refundable`; an amount above
the remainder is `400 amount_too_large`.

**Errors.** Codes are stable; messages are not.

| Status | `type` | `code` |
|---|---|---|
| 400 | `invalid_request_error` | `parameter_missing`, `parameter_invalid`, `parameter_unknown`, `amount_too_small`, `amount_too_large` (each with `param`) |
| 401 | `invalid_request_error` | `signature_invalid` |
| 404 | `invalid_request_error` | `resource_missing` |
| 409 | `idempotency_error` | `idempotency_key_reused` (the same key with other parameters) |
| 409 | `invalid_request_error` | `signature_replayed`, `exposure_cap_exceeded`, `quote_payment_received`, `quote_window_closed`, `quote_unexpected_state`, `deposit_not_refundable`, `paused`, `chain_frozen` |
| 429 | `invalid_request_error` | `rate_limit` (quote creations per account; unsigned quote reads) |
| 503 | `api_error` | `unavailable` (no fresh price, database unavailable) |
| 500 | `api_error` | `internal_error` |

The SDK retries `429`, `5xx`, transport errors, and `signature_replayed`, re-signing with the same
`Idempotency-Key`.

**Events** (Standard Webhooks, signed with the settlement key) are Stripe's Event object,
`{id: "evt_…", object: "event", type, created, data: {object}}`: `deposit.credited`,
`deposit.rejected`, and `deposit.refunded` (one per final refund) carry the deposit, and
`quote.expired` the quote. `data.object` is the object as the API returns it, rendered on the first
delivery attempt and stored, so every retry and replay sends the same body. Every event id is
`uuid_v5(NS, "{type}:{object UUID}")`, the object being the refund for `deposit.refunded`, so a
re-emission after a restore deduplicates for every type. `deposit.credited` is the fulfillment
event (§11); the others are informational and never change balances. Nothing is sent before
finality: the checkout page reads the quote's `payment`. The outbox does not order events, so
`quote.expired` can arrive after the `deposit.credited` of a late payment; receivers must act on
state (the deposit or quote they fetch), never on event order. Object changes are additive;
receivers must ignore unknown fields.

OpenAPI comes from `utoipa`; the SDKs are generated from it and ship with a runnable integration
example, a signing helper, and a versioning and deprecation policy. A sandbox (Sepolia, test
token, product credentials, scripted late/under/over/rejected scenarios) is available to
integrators before mainnet.

### Customer experience obligations (product UI)

These follow exchange and payment-processor conventions and are part of the integration
checklist:

| Topic | Requirement |
|---|---|
| Default flow | Quote first: amount input → locked price, exact token amount, single-use address, QR as an EIP-681 URI carrying token and amount, countdown to `expires_at`, and the rule for late or wrong-amount payments. |
| Warnings | Network name and chain id, full token contract, "only PHA on Ethereum", minimum deposit, and that below-minimum deposits are not credited. Never truncate addresses or hashes. |
| Waiting | After payment the user sees "received, N confirmations, final around hh:mm" from the quote's `payment` (`status: "seen"`), with a transaction-hash lookup and an explorer link; it is not credited and may still disappear in a reorg. Deposits are reported only once final. |
| History | Each deposit shows token amount, rate, valuation time, USD credited, transaction hash, status, and quote. |
| Quote page | Shows spread, that network and exchange withdrawal fees are the user's, the lock window, remaining limits, workspace name, promotion eligibility, and what happens on underpayment, overpayment, or late payment. Supports cancel and re-quote; the page is resumable by the quote id. |
| Underpayment | Shows the amount received, the shortfall, and a "top up the difference" re-quote; multiple payments are not accumulated against one lock. |
| Exceptions | Wrong asset or below minimum: "contact support"; the funds are held (§15) and finance may return them per the refund policy. Overpayment beyond tolerance is not an exception: it is credited at spot for the full amount (§9). Sanctions: "under compliance review, contact support"; the reason code stays server-side. A credit the product held (closed or suspended workspace, the product's caps): "under review, contact support", then a refund to an address the user supplies (§15). Paused: "deposits temporarily unavailable", address hidden. |
| After credit | Shows the new available balance, debt settled, and whether service resumed. |
| Notifications | Email or in-app notice on `deposit.credited`, `deposit.rejected`, `deposit.refunded`, and `quote.expired`; the waiting screen's "payment received, waiting for finality" comes from the quote's `payment`. |
| Support | Support staff can look up by transaction hash, address, quote, workspace, or order and see the full timeline; every case has an owner and a response target. |

### Deposit status for exchange users (product UI)

Exchange users expect one progress line per deposit. The product maps service states to these UI
states. The service reports a deposit only once it is final (§8); the first state comes from its
display-only pending view (§12): the quote's `payment` with `status: "seen"`. That view is not a
credit and can disappear in a reorg, and only routed tokens
appear in it; other tokens first show as `rejected(unsupported_asset)` once final. Drive the UI
from fetched state, never from webhook order.

| UI state | Service state | Copy |
|---|---|---|
| Detected, N confirmations | none yet: the quote's `payment.status` `seen` (display only) | "Payment detected: N confirmations. Final around {`estimated_final_at`}." When `matches_quote` is false, add: "This payment does not match the quote, so it will be credited at the rate when it becomes final." |
| Finalizing | `detected` | "Final on Ethereum. Checking the payment and fixing the rate." |
| Crediting | `confirmed`, or `credited` before the product has applied the credit | "Crediting your balance." |
| Completed | `credited`, `swept`, and the product's own credit recorded | "Credited $X at $rate." When a lock-address payment was valued at spot (late, wrong amount, second payment), add: "Credited at the rate when your payment became final because it did not match the quote." |
| Needs attention | `rejected` | By reason, below. The reason code itself is never shown. |

| `reason` | "Needs attention" copy |
|---|---|
| `unsupported_asset` | "This token is not accepted here, so it was not credited. Contact support to have it returned." |
| `below_minimum` | "This payment is below the minimum deposit of X, so it was not credited. Contact support; amounts at or above the refund minimum can be returned." |
| `out_of_bounds`, `out_of_range` | "This payment is outside the deposit limits, so it was not credited. Contact support to have it returned." |
| `sanctioned` (and `product_refused` on historical deposits) | "This payment is under compliance review. Contact support." |

### Quote and address copy (product UI)

- Quote page, next to the single-use address: "Paying from an exchange? Exchanges may hold new
  withdrawal addresses and deduct withdrawal fees; the amount received must still equal the
  quote, or it is credited at the rate when it becomes final."
- QR codes: a quote's QR is an EIP-681 URI (token and amount), always shown with copy-address
  and copy-amount buttons for wallets and exchanges that do not read the URI.
- When a quote's `expires_at` has passed, hide its QR code and address and show "Payment
  window closed, awaiting finality. A payment sent in time is still credited at the quoted
  price." Offer a re-quote; the quote stays `open` until chain-time expiry (§9).
- Network warning on every address: "Ethereum mainnet only. Payments sent on any other network
  are not credited." Support handles such a payment with the
  [wrong-network deposit runbook](../deploy/runbooks/wrong-network-deposit.md).

## 13. Reconciliation

The reconciler runs every 10 minutes and stores each finding once; repairs are silent, every
other finding raises `TopupReconciliationMismatch` (§16).

| Check | Action |
|---|---|
| Finalized transfer to our address with no deposit row, in the range the scanner has committed | insert `detected` |
| `credit_minor` ≠ recomputation from stored inputs | alert, block flush |
| Deposit with no `flush_id` but a confirmed `flushed` row at a later log position | link it (replay of stored events) |
| Address balance ≠ Σ deposits − Σ `flushed.amount_atomic`; treasury inflow from our forwarders ≠ Σ `Flushed` events | alert |
| `addressOf(salt)` on chain ≠ stored address | freeze chain, alert |
| After a restore, in the read-only restore-check instance (§14) | the checks above, on the restored ledger alone: the service's record is authoritative for its credits, so the restore asks the product nothing and does not depend on it being reachable |

The log checks (missing deposits, treasury inflow against `Flushed` events) are incremental: each
resumes from a durable cursor, reads at most 64 windows of 2 000 finalized blocks per
round, and stores its progress after every window, so a round reads only what finalized since the
last one, and a restart or a failed round resumes where the stored progress ends until the whole
history has been covered once. A round's reads run one at a time on provider A. The first round
after a restart runs while every other task starts on the same provider, so a provider refusal
that asks for a retry (HTTP 429, JSON-RPC `-32005`, and the other rate-limit answers alloy
classifies) is retried within the round with exponential backoff and jitter, up to six retries
and at most 32 s of backoff per read. Any other failure, or a refusal outlasting the retries,
fails only its check and withholds the round's heartbeat; the next round runs it again.

A block (`block flush` for one address, `freeze chain`) stays until an operator lifts it with the
admin-signed `POST /v1/admin/reconciliation-blocks/{block_key}/lift {reason}` once the cause is
investigated and signed off; the daily report lists active blocks. Lifting is manual: the service
does not re-check first, and a finding that still reproduces blocks again on the next round. The
lift writes `audit` with the reason and the removed block in the same transaction.

## 14. Configuration and deployment

One route file per chain and asset pair, with its chain settings inline, in the compose, hence
attested. The file names only what differs per route or environment: route name and version,
product, chain id, forwarder factory, treasury, asset symbol, contract, and decimals, price
sources, and the policy limits (minimum credit, maximum deposit, refund floor, exposure caps).
Every other value is a code default, overridable under its key in the same file, and as attested
as the file because the image digest is part of the compose hash. `topup route show FILE` prints
the resolved route, every value explicit (JSON, itself a valid route file); preflight reads the
defaulted addresses from it. The defaults and why:

| Value | Default |
|---|---|
| `chain.implementation` | the factory's first `CREATE` (nonce 1), which its constructor deploys; startup verifies `implementation()` on chain (§4) |
| `chain.sanctions_oracle` | the Chainalysis oracle published for the chain (Ethereum and most EVM chains `0x40C5…aC8fb`, Base `0x3A91…D739B`); required on any other chain, such as Sepolia |
| `chain.rpc_providers` | `[provider-a, provider-b]`, whose URLs are `TOPUP_RPC_PROVIDER_A_URL` and `_B_URL` |
| `chain.operator_key_version` | 1; bumped only after an operator rotation (§15) |
| `chain.flush.schedule`, `max_gas_ratio_bps`, `max_fee_per_gas_wei`, `replacement_bps` | `0 */6 * * *`, 200 (2% of value), 500 gwei (a runaway-fee guard), 12 500 (a 25% bump) |
| `chain.flush.native_price_asset` | `eth` on Ethereum, Sepolia, and Base; required elsewhere |
| `chain.flush.min_operator_balance_wei` | 0.05 ETH (staging overrides 0.01 ETH, its operator's float) |
| `pricing.mode`, `pricing.check.fx` | `spot`; Kraken `USDT/USD` for a USDT-quoted market, required otherwise |
| `pricing.max_age_s`, `max_deviation_bps`, `max_fx_deviation_bps` | 120 (two Coin Metrics intervals), 100, 50 |
| `limits.min_deposit_atomic`, `limits.min_flush_atomic` | 0: `min_credit_minor` rejects dust, and the gas-ratio rule governs flush economics *(policy: finance confirms before production)* |
| `quote.window_s`, `spread_bps`, `tolerance_bps`, `max_creations_per_minute` | 900, 50, 100, 10 |
| `quote.amount_decimals` | 4, or `asset.decimals` if fewer: a quote asks for, say, `273.9185` PHA rather than 18 decimals; at most `asset.decimals` |
| `alerts.stuck_after_s` | detected 1 800, confirmed 1 800, credited 172 800 (credited waits for the six-hourly, gas-gated flush) |
| `unit_decimals` | 2 (USD cents) |

The defaults are the pilot's numbers *(policy)*: finance confirms each, including the zero token
floors, before production, and a route overrides any it does not accept.

Only `finalized` finality is supported, so it is not configurable. A product's key id is
`{product}/v1`; the database stores only the product's slug, webhook URL, and public key.
Changing a value, including a default, is a new version and compose hash; deposits keep the
version that created them. Bumping `operator_key_version` is such a new version; bump it only after the admin Safe has granted the
new operator address (§15 Rotation). Pause flags are the only runtime-mutable state. Every
other setting (RPC URLs, admin key, object storage location, public origin, Sentry environment)
is rendered into the compose, so it is attested too; a keyed RPC URL is attested with a `{key}`
placeholder. The only dstack encrypted environment variables are the object-storage credentials,
the Sentry DSN, and the RPC providers' API keys that fill those placeholders. The in-CVM database's passwords
are the hex of `get_key("db/owner/v1")` and `get_key("db/app/v1")`, handed to PostgreSQL and its
clients as tmpfs files (`POSTGRES_PASSWORD_FILE`, `PGPASSFILE`), identical on every CVM of the app
id. Startup refuses to run without the dstack
socket, two RPC providers, or the on-chain contract checks of §4.

All enabled versions are loaded at startup. The highest enabled version of a route is current for
new API operations, while older versions remain available for historical deposits. A chain is
scanned only while it has a loaded route, and its quotes expire only by its scanner's cursor
(§9), so a route version or a chain's last route is removed only after its open locks and
in-flight deposits have resolved (`deploy/runbooks/route-retirement.md`).

The route's `chain.flush` settings own the flush policy: the planning cron, `max_gas_ratio_bps`,
native gas-price asset id, maximum EIP-1559 fee, replacement fee bump, and the operator gas
reserve `min_operator_balance_wei` (§16).
Gas policy compares gas-token value and token balance value in USD using separate reference
rates. Changing any of these fields requires a new attested configuration version. Engineering
limits that do not decide money are code constants: RPC timeout, replacement delay (3 blocks),
gas-limit buffer, nonce-recovery window, estimation exclusion retry delay, and maintenance
interval. Reads over every issued address (token balances, native balances, `addressOf`) are
aggregated through the canonical Multicall3 (`0xcA11bde05977b3631167028862bE2a173976CA11`) with
`aggregate3` and `allowFailure = false`, one `eth_call` per 200 calls (a code constant bounding
calldata and gas), at the block each read needs (`finalized` for custody reconciliation). They
never use JSON-RPC batches, which public providers throttle far below their single-request limits
(Tenderly's public gateway refuses a batch of more than five `eth_call`s), nor one request per
address, which grows with every address ever issued. The price scale (8) and the Coin Metrics
metric (`ReferenceRateUSD`, 1m) are fixed by §8 and §11, not configured.

```yaml
services:
  topup:    { image: ghcr.io/phala-network/phala-pay@sha256:…, command: ["topup", "run"] }
  postgres: { image: ghcr.io/phala-network/postgres-walg@sha256:…,     # postgres:18 + WAL-G
              volumes: [pgdata:/var/lib/postgresql] }   # archive_timeout=60, archive_command=walg-cron wal-push %p
  backup:   { image: ghcr.io/phala-network/postgres-walg@sha256:…, command: ["walg-cron", "backup-push", "0 3 * * *"] }
```

`TOPUP_PUBLIC_ORIGIN` is the service's public scheme and authority, `https://<custom domain>`
(no path), whose TLS the official dstack-ingress terminates inside the CVM with the certificate
evidence published (`deploy/README.md`, "Custom domain"); `topup run` refuses to start without a
valid value.

Postgres on the CVM's encrypted disk; WAL-G daily base backups and continuous WAL with
`archive_timeout=60` and a one-row heartbeat a minute, encrypted with `get_key("backup/v1")`
before leaving the CVM, one key per backup prefix (RPO ≤ 1 min, RTO ≤ 1 h). Restore is a
bootstrap from backup: a new instance of the same app boots the attested restore-check variant of
the compose, whose PostgreSQL restores and promotes with archiving off, whose `topup` is
read-only on its own port, and which runs the post-restore check (§13) and reports it on
`/healthz`; resume upgrades that instance to the service compose (`deploy/RESTORE.md`). A
restore loses at most the RPO window. A deposit whose `credited` transition was lost is rebuilt
and credited again with the same `deposit.credited` event id and deposit id, so the product
ignores the repeat; a lock-priced deposit gets the same amount, while a spot-priced one is
re-priced, and the product keeps its first credit and reports a differing amount (§11). The
restore drill runs weekly in CI on a local stack; the staging drill restores staging's real
backups. Addresses need no restore because salts derive from product data. Ingress via the
dstack gateway to dstack-ingress, which terminates TLS for the custom domain in the CVM; egress limited to providers, price sources, object storage, product URLs, and
Sentry. The CVM runs the non-dev OS image `dstack-0.5.9`, the latest dstack release a Phala
Cloud node offers; deploy preflight refuses any other image and a node set that does not offer
it. Upgrade = reproducible build → digest (Release images on `main`) → compose hash → CI
deploy (the dispatcher is accountable; no approval gate) → attested read-back. Keys come from Phala Cloud's
KMS, with no on-chain compose-hash allow-list: funds go only to the immutable treasury, so a
malicious upgrade could cause downtime, read service data, or sign credits no deposit backs, up
to whatever caps the product keeps (§3, §11), but not move funds; the attested compose hash makes
it detectable.
`GET /v1/attestation?nonce=` returns the dstack attestation (TDX quote and event log) of
`/Attest` with `report_data = sha256(nonce ‖ settlement_pubkey ‖ record_1 ‖ … ‖ record_n)`, where `operators` lists each configured chain's
flusher operator in ascending `chain_id` order (`chain_id`, `operator_key_version` from the
chain's current routes, `keyid = operator/v{n}`, and the address the flusher signs with) and
record `i` is the 32 bytes `chain_id` (u64 big-endian) ‖ `operator_key_version` (u32 big-endian) ‖
address. With no operators this is the original `sha256(nonce ‖ settlement_pubkey)`. Verifiers
run the official dstack verifier of the pinned release on it (`deploy/dstack-verifier.sh`: quote
and TCB, RTMR3 event-log replay, OS image; then the app id and deployed compose hash), check that
the verified report data is this hash zero-padded to 64 bytes, and then pin
`(keyid, public key)`; the owner grants `OPERATOR_ROLE` to, and funds, only an operator address
verified this way, since a production CVM exposes no logs or shell.

## 15. Operating policies

| Topic | Rule |
|---|---|
| Addresses | Every address is a quote's, single-use; a later payment to it is credited at spot. Legacy persistent addresses stay monitored by the finalized scanner and are never issued again. |
| Dust and mistakes | Below-minimum and unsupported-asset deposits are recorded, visible, and not credited. Rejected deposits of a routed token are flushed to the treasury with everything else; an unsupported token stays in its forwarder, since the flusher sweeps only routed tokens, until a separately reviewed Safe flush. |
| Refunds | Refundable: wrong token; below the minimum credit but at or above `min_refund_atomic` *(policy)*; rejected for any reason other than sanctions; funds arriving after the workspace closed. A credited deposit is refunded only on the product's request, for a credit the product did not apply or has reversed; an overpayment beyond tolerance is credited at spot for the full amount (§9) like any other credit. Not refundable: sanctioned funds and dust under `min_refund_atomic`. The user requests a refund with a destination address they control (never defaulted to `from_address`, which may be an exchange hot wallet); finance approves and executes from the treasury Safe; the service records the transaction, emits `deposit.refunded`, and reconciles it. Refunds are in the original token net of gas, within a published processing time. |
| Workspace closure | Unused credit and in-flight deposits follow the product's closure policy; the old address stays monitored, and later funds are held for refund: the product holds a `deposit.credited` for a closed workspace instead of crediting it and requests its refund (§11); the service has no closure check of its own. |
| Compliance | Direct sanctions screening from the pilot; region and Travel Rule applicability decided in Phase 0; KYT adapter and a compliance case flow (customer information request, reviewer role, response time, disposition) before GA. Record requests follow a documented verification, approval, and delivery procedure. |
| Fees and exposure | Gas is a service cost; credit is never reduced. Treasury bears price exposure between valuation and flush, and open quote exposure up to the caps. |
| Rotation | Operator key: grant `operator/v2`, revoke `v1` (admin Safe); flush nonces are tracked per operator address, so the new key starts at nonce 0 without conflict. Settlement key: add `settlement/v2`; products accept both for 30 days. Product key: the admin replaces the stored public key (`PUT /v1/admin/products/{slug}`), a hard cut: requests are verified against one stored key under the one key id the product's routes name (§14), so the old key fails from that commit; the key id is unchanged. Backup key: a new domain and a new prefix; the old prefix is kept until the new one holds a full retention window. |
| Retention | Deposits, transitions, audit, and the read-only history of the retired settlement protocol (`settlements`): 7 years *(policy)*, append-only. |
| Kill switches | Pause scopes (`quotes`, `settlement`, `flush`, `refunds`) at account, product, or route level. Each scope's customer-facing effect is documented and shown; `settlement` stops crediting (deposits wait in `confirmed`); pausing never rolls back a credited fact. Incidents are announced on the product status page with affected routes and updates. |
| Runbooks before pilot | operator key compromise, product key compromise, provider disagreement, price outage, outbox backlog (undelivered credits), restore, treasury change, gas refill, refund execution, rejected funds at treasury. |

## 16. Observability and tests

A production CVM has no logs, no shell, and no metrics collector, so Sentry is the one monitoring
pipeline. Errors and panics are events. An alert is a warning tagged with its name and
low-cardinality grouping tags (route, state, check, chain, scope), fingerprinted by them and
linked to its runbook: `TopupDepositStateAgeExceeded` (age in state past the route's
`alerts.stuck_after_s`), `TopupReconciliationMismatch`, `TopupLockExposureNearCap`,
`TopupLockExpiryFailing`, `TopupUnsupportedInflows`, `TopupOperatorGasReserveLow` (the operator's
native balance below `min_operator_balance_wei`, checked on each flusher maintenance tick), and
the flusher's `Reverted`, `IsolatedAddress`, `MissingConsumedReceipt`, `PlanningExcluded`,
`FeeCapReached`, `NativeBalance`, and `OperatorRoleMissing`. Each loop checks in to a Sentry Crons
monitor, which pages on scanner lag, backup age over 2 minutes, a failed reconciliation check, and
any stopped loop; a Sentry Uptime monitor watches `/healthz`. Business state (deposits by state and
age, unflushed balance, open lock exposure, undelivered `deposit.credited` events and their age,
flush planning, reconciliation) is in the daily admin
report (`GET /v1/admin/report/daily`). Log lines and their spans (`deposit_id`, `chain_id`,
`state`, `attempt`) serve local stacks.

Tests. `core`: exhaustive transitions, `proptest` on credit math, CREATE2 math against
Foundry, route schema. Contracts: Foundry unit, fuzz, and invariant tests (`flush` can only
pay the treasury; clone address prediction; ETH path; reentrancy with a hook token).
Integration on `anvil` + Postgres: happy path; duplicate logs; provisional evidence corrected
after provider agreement; racing pumps; stale lease; stale or divergent prices; sanctions hit;
credit and its `deposit.credited` event (payload and derived id) in one transaction; a repeated
event id stored once; lock exact, over, under, late, double payment; batch flush with
replacement, reverted flush, and operator rotation; flush carrying pending and rejected
deposits; deposit backfilled after its flush; deposit arriving while a flush is unconfirmed; a
restore check that asks the product nothing and keeps recorded credits; the migration that
retired `cleared`. The reference product's tests cover the §11 obligations (credit once across
redeliveries, forged deliveries refused, holds); `topup-sdk send-test-event` checks any receiver;
`signer::dstack`
against the simulator when explicitly enabled; attestation report-data construction against a known vector and the simulator
response when available.

## 17. Delivery

**Phase 0**: deterministic deployment on Sepolia and mainnet;
finance Safe verified on each chain; two RPC providers; object storage; treasury; policy
numbers; compliance determination of region, Travel Rule, and KYT timing; refund policy
signed off by finance.

**Phase 1, capped pilot**: full pipeline including flush and quote-first deposits, on Sepolia
then mainnet with a per-deposit `max`, small lock-exposure caps, product-side caps, and
allow-listed accounts. Acceptance:
address issued without an operator and recomputable by the product; a quote-first deposit is
credited at exactly the amount shown to the user, and a late or wrong-amount payment at spot;
deposit recovered after restart and provider interruption; credit only after two-provider
finality; duplicates and
concurrency yield one ledger mutation; outages only delay; balances flush and `Flushed`
events match; reconciliation repairs the two safe cases and alerts on the rest; restore issues
no duplicate; every deposit has a full evidence timeline; support lookup and `nudge` used on a
real case; one refund executed end to end; the daily finance report delivered; runbooks
exercised once.

**Phase 2, GA**: bounds and lock-exposure caps raised; KYT adapter and compliance case flow;
dashboards; CSV and accounting exports; webhook replay and delivery logs; notification
preferences; reconciliation exception queue with sign-off; localization.

**Phase 3**: Base PHA and USDC routes through chain and route files; same addresses on both
chains.

Issue #2 is split along Phase 0 and 1; the Phala Cloud endpoint is filed in the monorepo.

## 18. Product feature map

Ownership: **S** service, **P** product (Phala Cloud UI and billing), **F** finance, **C** compliance.

| Feature | Owner | Pilot | GA | Later |
|---|---|---|---|---|
| Quote-first checkout: spread and fee disclosure, exact amount, EIP-681 QR, countdown, resume by quote id, cancel, re-quote | S+P | ✓ | | |
| Underpayment shortfall and top-up re-quote; overpayment beyond tolerance credited at spot | S+P | ✓ | | |
| Wallet and exchange payment guidance, mobile deep link, copy fallback | P | ✓ | | |
| Waiting screen with distinct stages, last update, when to ask for help | P | ✓ | | |
| Pre-finality "seen" payment view (the quote's `payment`; the payer's `payment_status`) | S | ✓ | | |
| Deposit history with filters and pagination; receipt per deposit | S+P | ✓ | | |
| CSV export; accounting and cost-basis export; valuation evidence bundle | S+F | | ✓ | |
| Notifications on credited, rejected, refunded, lock expired; preferences and history | P | ✓ (basic) | ✓ | |
| Support lookup by hash, address, lock ref, workspace, order; case owner and response target | S+P | ✓ | | |
| Manual `nudge` of a deposit; audited | S | ✓ | | |
| Refund policy and treasury refund workflow (request → approve → Safe → record → notify) | S+F+P | ✓ | | |
| Limits page: caps, remaining, reset time; allowlist and limit-increase requests | S+P | ✓ | | |
| Workspace roles for deposit, history, export, refund request | P | ✓ | | |
| Post-credit view: balance, debt settled, service resumed; low-balance prompt to prefilled quote | P | ✓ | | |
| Promotion eligibility display and interplay rules | P | ✓ | | |
| Workspace closure and late-funds handling | S+P+F | ✓ | | |
| Daily finance report; treasury, exposure and PnL dashboards | S+F | ✓ (report) | ✓ | |
| Reconciliation exception queue with sign-off | S+F | | ✓ | |
| Sanctions screening; KYT adapter and compliance case flow; region and Travel Rule determination | S+C | ✓ (screening, determination) | ✓ (KYT, cases) | |
| Record request and privacy procedure | C | | ✓ | |
| Pause scopes with customer-facing effect; status page and incident communications | S+P | ✓ | | |
| Localization, currency and time display | P | | ✓ | |
| SDK with signing helper, idempotent client, examples; versioning policy; sandbox | S | ✓ | | |
| Webhook delivery log, test send, replay | S | admin replay | ✓ | |
| Multi-product tenancy administration; self-serve product onboarding | S | | | ✓ |
| Sender address book and source whitelisting | S+P | | | ✓ |
| Built-in token purchase, withdrawal, trading account | — | | | never |
