# Crypto Top-up Service — Design

Status: v5. Single specification and implementation design. Numbers marked *(policy)* are set
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
| Idempotent HTTP | `Idempotency-Key` (IETF httpapi draft): `422` on payload mismatch, `409` on concurrent processing | Business outcome is always in a `200` body (§11) |
| Request signing | RFC 9421 HTTP Message Signatures, ed25519, `content-digest` | — |
| Webhooks | Standard Webhooks | — |
| Money | Integer minor units; 8-decimal scaled prices | Precision is an application choice |
| Backup | WAL-G base backups plus continuous WAL, `archive_timeout` bounding RPO | — |
| TEE | dstack KMS derivation and attestation verification flow | — |

## 1. Goal

A private service, called by the Phala Cloud billing backend, that turns finalized and
screened deposits of configured tokens into USD credit and credits the product at most once
through a signed HTTP call. Two ways to deposit, both first-class:

- **Quote first (default UI).** The user states a USD or token amount, receives a locked
  price, an exact token amount, a single-use address, and a countdown, then pays. This is the
  checkout model of Coinbase Commerce and BitPay.
- **Persistent address (advanced).** Any amount, any time, valued at the price observed when
  the deposit reaches finality. This is the exchange deposit model.

Customer contract: *tokens are converted to non-transferable Phala Cloud USD credit at the
published rate observed when the deposit reaches Ethereum finality; the USD value is fixed
after crediting.*

Success: eligible deposits are credited exactly once with no operator step, also after any
outage; balances flush to the treasury automatically; chain, service, and product ledger
reconcile.

First route: Ethereum Mainnet PHA → Phala Cloud USD. New tokens and EVM chains are new route
files; new products are new settlement endpoints.

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
   price sources; the product enforces its own limits and verifies chain evidence.

## 3. Trust model

The service runs in a dstack confidential VM. The host and cloud provider cannot read keys or
alter code without changing the attested measurement; products verify by attestation which
code holds the settlement key; the database is inside the boundary.

Deposit addresses are forwarders with an immutable treasury, so **a full compromise of the
service cannot redirect deposited funds**. A compromised service could still forge settlement
requests; that is bounded by product-side obligations (§11): independent per-deposit and
per-period caps, and verification of the cited log against the product's own RPC. Residual
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

- `salt = keccak256(abi.encode(product_slug, external_id, version))` for persistent
  addresses; `keccak256(abi.encode(product_slug, external_id, "lock", product_lock_ref))` for
  rate locks. The product holds every input, so any address can be recomputed with no service
  state. The API returns the inputs with the address.
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
crates/adapters   chain::evm, signer::dstack, pricing::{coinmetrics,binance,kraken}, risk::oracle, settlement::http
crates/topup      binary: db, pump, scanner, flusher, outbox, reconciler, api, cli
contracts/        Forwarder.sol, ForwarderFactory.sol, deploy scripts, Foundry tests
config/routes     route files (attested)   deploy/  compose + Dockerfile   tests/  integration + contract
```

## 6. Schema

Amounts are `numeric(78,0) CHECK (>= 0)` mapped to `U256`; `transitions` and `audit` are
append-only. Physical addresses belong to an account and a chain; routes are selected per
deposit by `(chain_id, asset_contract)`.

```text
products      id, slug, webhook_url, pubkey, paused_scopes text[]   -- settlement URL, key id: route (§14)
accounts      id, product_id, external_id, paused_scopes text[]    UNIQUE (product_id, external_id)
              -- scopes: quotes | addresses | settlement | flush | refunds; empty = active
addresses     id, account_id, chain_id, kind (persistent|lock), version, lock_ref, salt, address, retired_at
              UNIQUE (chain_id, address)
              UNIQUE (account_id, chain_id) WHERE kind = 'persistent' AND retired_at IS NULL
rate_locks    address_id PK, route, amount_atomic, price_scaled, credit_minor, expires_at,
              consumed_by (deposit_id) UNIQUE
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
settlements   deposit_id PK, product_id, key, payload jsonb, status (intent|sent|accepted|rejected),
              destination_tx_id, receipt jsonb, resend_forbidden bool, sent_at
              UNIQUE (product_id, destination_tx_id) WHERE destination_tx_id IS NOT NULL
flushes       id, chain_id, token, operator, nonce, tx_hash, block_number,
              status (planned|sent|confirmed|reverted), receipt jsonb
              UNIQUE (chain_id, operator, nonce)
flushed       flush_id, address_id, amount_atomic, block_number, log_index   -- one row per Flushed event
              PRIMARY KEY (flush_id, address_id)
refunds       id, deposit_id, amount_atomic, to_address, tx_hash, status (requested|approved|sent|confirmed),
              requested_by, approved_by, created_at                 -- executed from the treasury Safe; recorded here
outbox        id, event_type, payload jsonb, next_attempt_at, delivered_at, response jsonb
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
detected → confirmed → cleared → credited → swept
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
| `detected → confirmed` | First, `GET` the product by the deterministic key: an existing answer restores the original payload and business state (`credited` or `rejected`) directly, before any quoting or local decision, so a deposit rebuilt after a restore can never diverge from what the product already did. Then, from both providers: `finalized ≥ block_number`, same block hash, same log. While `detected`, evidence is provisional: if both providers agree on different canonical evidence for the same event identity, the row is corrected. If both are final past the row and neither has the log, the step retries with `log_absent_at_finality`, never a rejection. In the same step, fetch the quote (§8) and store `valuation_at`, `price_scaled`, `credit_minor`, `quote`. Below `min_credit_minor` → `rejected(below_minimum)`. |
| `confirmed → cleared` | `isSanctioned(from)` on both providers at a recorded block; `min ≤ amount ≤ max` *(policy)*; account and product not paused (paused → `Wait`, never a rejection). |
| `cleared → credited` | Signed `POST` (§11). Body `rejected` → `rejected(product_refused)`. On an unknown result, `GET` before any resend. |
| `credited → swept` | `flush_id` is set: a confirmed `flushed` row exists for the deposit's address and token at a log position `(block_number, log_index)` greater than the deposit's. Evaluated on flush confirmation and on every deposit insert, so backfilled deposits resolve too. |

## 8. Chain, valuation, screening

**Scanner** per chain: read `finalized` from provider A; fetch `Transfer(*, our addresses)`
from any contract in windows ≤ 2 000 blocks and ≤ 1 000 addresses; insert with
`ON CONFLICT DO NOTHING`; advance the cursor after commit. New addresses backfill from
creation (the chain's committed cursor when the address is issued); retired and lock addresses
stay in the filter. Native ETH is a balance check at flush time. Each chain's finality rule is
declared in the route's `chain` settings (Ethereum: `finalized`); a chain is enabled only after
its rule is reviewed. Later option: Helios as one provider.

**Head scan (display only)** per chain, on provider A, every 12 s (or the scanner poll interval
if shorter): read non-zero `Transfer` logs emitted by the chain's routed token contracts to
watched addresses in `[finalized + 1, latest]` and, in one transaction, upsert the rows seen into
`pending_transfers` and delete rows in that range not seen this time (reorged, or no longer
watched). Other tokens are never requested, so they cannot create pending rows or
notifications; they appear only after finality, as `rejected(unsupported_asset)`. Block times are
read by block hash. The head scan reads the finalized cursor `FOR SHARE`, and the finalized
scanner deletes rows at or below its cursor in the transaction that advances it, so a transfer
moves from pending to deposit atomically and no row below the cursor is written afterwards.
Pending rows never feed deposits, transitions, locks, exposure, settlement, or reconciliation;
lock amount and timeliness are computed when read, never stored. When the head scan sees
`finalized` advance it wakes the finalized scanner, and a confirm step waiting for provider B's
finality retries after 12 s instead of the regular wait interval. While reconciliation has frozen
a chain its head scan stops too, so the pending view stops updating.

Watched addresses: lock addresses whose lock is neither consumed nor cancelled, until one hour
after `expires_at`, and persistent addresses. Open locks are bounded by the exposure caps (each
reserves at least `min_credit_minor` against the global cap) and, for the hour after expiry, by
the per-account creation rate limit; any number is requested in batches of 1 000. When
persistent addresses exceed one 1 000-address request, only those issued or fetched by the
product (`requested_at`: address issuance, `GET` of the address, or `GET …/pending-deposits`) in
the last 24 hours are watched, most recent first, capped at 1 000 across all products (a global
cap, not a per-product share; revisit when a second product goes live).

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

## 9. Quote-first deposits (rate locks)

Invoice model, enabled from the pilot, with this service's exception profile:

- `POST …/rate-locks {amount_minor | amount_atomic, product_lock_ref}` returns
  `{address, amount_atomic, price_scaled, credit_minor, expires_at, eip681_uri, salt_inputs}`.
  `price_lock = price_spot / (1 + spread)` with `spread = spread_bps / 10 000` *(policy)*;
  when the user states USD, the token amount is rounded up. `expires_at = now + window`
  *(policy)*. Locks count against open-exposure caps per account, per product, and global
  *(policy)*, reserved atomically at creation; creation is rate-limited per account. Repeating
  a `product_lock_ref` returns the stored lock (a different amount is `409
  idempotency_mismatch`), also while `quotes` is paused or the route disables rate locks.
- The lock is consumed by the first deposit to its address whose `block_time ≤ expires_at`,
  `asset` matches, and `|amount − locked| ≤ lock_tolerance_bps` *(policy)*; consumption is a
  single `UPDATE … WHERE consumed_by IS NULL`. That deposit is valued at `price_lock` and the
  product receives exactly the `credit_minor` it showed the user.
- Any other deposit to a lock address (late, wrong amount, second payment) is valued at spot
  and still credited; the product shows this rule before payment.
- Expiry uses chain time, like eligibility. A lock expires unconsumed, releasing its exposure
  and emitting `rate_lock.expired`, only once the chain's scanner has committed through a
  finalized block whose time is past `expires_at` (the finalized head's time, read with the
  head, is stored with the cursor) and no deposit mined inside the window still awaits its
  confirm step. A payment mined inside the window is therefore consumed at the lock price and
  never reported as expired. Exposure stays reserved until finality, about 15 minutes after
  `expires_at`, and longer while the scanner is stalled (§16). Until then the API
  shows the lock `open` with `remaining_seconds = 0`, and cancellation is refused once the
  window has closed (`409 window_closed`). A lock whose address has received any deposit, even
  a rejected one, can no longer be cancelled (`409 pending_payment`).
- Exposure counters sum `credit_minor` across routes, so every rate-lock route must use the
  same `destination.unit_decimals`; the service refuses to load routes that differ.
- A "quote, then pay to the persistent address" variant is deliberately not offered: matching
  a lock by amount alone is ambiguous, and the single-use address is the processor-standard
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
rebroadcast the same signed transaction. Gas is a service cost and never reduces credit.

## 11. Settlement contract

RFC 9421 signatures (ed25519, `keyid = settlement/v1`) over `@method`, `@target-uri`,
`content-digest`, `idempotency-key`, `created`; receivers reject `created` outside ±5 minutes
and verify the digest. Every resend keeps the payload and regenerates `created` and the
signature.

```http
POST {settlement_url}
Idempotency-Key: "deposit:<deposit_id>"

{ "version": 1, "idempotency_key": "deposit:…", "account_id": "<external_id>",
  "unit": "USD", "amount_minor": "1234", "source": "crypto_deposit",
  "evidence": { "chain_id": 1, "asset_contract": "0x…", "route": "…", "route_version": 1,
                "tx_hash": "0x…", "log_index": 12, "to": "0x<forwarder>", "amount_atomic": "…",
                "price_scaled": "…", "price_scale": 8, "valuation_at": "…", "lock_ref": null } }
```

| Product answers | Service does |
|---|---|
| `200 {status:"accepted", destination_tx_id}` | `credited` |
| `200 {status:"processing"}` or `409` | stay, poll `GET` |
| `200 {status:"rejected", reason}` | `rejected(product_refused)` |
| `422` (same key, different payload) | invariant violation: stay, alert |
| anything else | `GET {settlement_url}/{key}` first; if unknown, resend; alert on age |

`GET {settlement_url}/{key}` returns the stored status, `destination_tx_id`, and the
**original accepted payload**. Every deposit's first step `GET`s by key before quoting (§7),
so a deposit rebuilt after a restore adopts the product's fact and the original pricing inputs
instead of re-quoting or re-deciding; a `422` is handled the same way. The product's answer
is authoritative for the credited fact.

Product obligations, checked by the conformance suite:

1. Verify the signature against the pinned key (public key bytes and `keyid` pinned together).
2. Keep idempotency records for the life of the account; never expire them.
3. Commit before answering `accepted`; mutate the ledger once under concurrency.
4. Enforce its own per-deposit and per-period caps, independent of the service.
5. Verify the cited log against its own RPC: it exists at `tx_hash`/`log_index` in a
   finalized block, was emitted by the approved `asset_contract` for that route, `to` equals
   the forwarder address the product computed for that account, and `amount_atomic` matches.
   Transient chain-read failures must not produce a stored rejection.
6. Recompute `deposit_id = uuid_v5(NS, "{chain_id}:{tx_hash}:{log_index}")` from the
   evidence and require `idempotency_key == "deposit:" + deposit_id`, so one chain event can
   never be credited under a second key.

The suite (`docs/conformance.md`) also requires, in the product's test environment only, a
ledger observation hook (`GET {settlement_url}/_conformance/ledger/{account_id}`) and a restart
during the run, so single mutation and durability are observed rather than inferred.

Phala Cloud: find-or-create an `Order` (`provider = crypto_topup`, `order_flow_code =
'crypto-top-up'`, `provider_order_id = key`, partial unique index on `(team_id,
provider_order_id)` for that flow), credit transaction tagged `funding_source =
crypto:<asset>:<chain>`, then `complete_order_payment` in the same transaction; return
`credit_transaction_id`. Non-card top-ups skip the welcome promotion by existing rule. Filed
as a monorepo issue.

## 12. API and events

Product requests use the same signature scheme with the product's key; paths use the product's
`external_id`; every request is checked for tenant ownership. Address responses include the
salt inputs (`product_slug`, `external_id`, `version` or `lock_ref`) so the product can
recompute any address without the service. The admin key can only issue products, pause and
resume, nudge, and drive the refund workflow; each change writes `audit`. The verifier rebuilds `@target-uri` from the configured public origin
(`TOPUP_PUBLIC_ORIGIN`, §14) and the request's path and query, never from `Host` or
`X-Forwarded-*`, so signers sign the public URL they call. Each deployment (sandbox, staging,
production) must pin a distinct product key: signature single-use is recorded per database, so a
shared key would let a signed request be replayed within the freshness window against another
deployment that shares its public origin (for example a replacement or restored instance).

```text
POST   /v1/products/{p}/accounts
POST   /v1/products/{p}/accounts/{ext}/deposit-address           persistent; GET same
POST   /v1/products/{p}/accounts/{ext}/deposit-address/rotate    version + 1; old stays valid
POST   /v1/products/{p}/accounts/{ext}/rate-locks                 single-use address + locked price
GET    /v1/products/{p}/accounts/{ext}/rate-locks/{ref}           resume a checkout page
DELETE /v1/products/{p}/accounts/{ext}/rate-locks/{ref}           cancel an unpaid lock; later payments credit at spot
GET    /v1/products/{p}/accounts/{ext}/deposits?state&from&to&cursor
GET    /v1/products/{p}/accounts/{ext}/pending-deposits           seen above finalized; not deposits
GET    /v1/products/{p}/deposits/{id}
GET    /v1/products/{p}/deposits?tx_hash= | address= | lock_ref=  support lookup
GET    /v1/products/{p}/accounts/{ext}/limits                     caps, remaining, reset time
POST   /v1/products/{p}/accounts/{ext}/pause | resume {scopes}
POST   /v1/products/{p}/deposits/{id}/refund-requests {to_address, amount}   finance approves and executes (§15)
GET    /v1/attestation?nonce=…                                    settlement key and flusher operators (§14)

GA:    GET  …/deposits.csv        POST …/webhooks/replay {event_ids | since}     GET …/webhooks/deliveries

POST   /v1/admin/products {slug, public_key, webhook_url}   key id, settlement URL: route (§14); same values → same product, different → 409
POST   /v1/admin/routes/{r}/pause | resume {scopes}
POST   /v1/admin/deposits/{id}/nudge          next_attempt_at = now; no state change; audited
POST   /v1/admin/refunds/{id}/approve | record {tx_hash}
GET    /v1/admin/report/daily                 treasury, unflushed, open locks, rejected holds, global lock exposure
```

Signatures are single-use within the acceptance window. `rotate` is idempotent on
`from_version`.

Pending view (display only, §8). A rate lock carries an optional `payment`, chosen by the §9
consumption rule: the deposit that consumed the lock; otherwise the first payment that would
consume it (asset, `in_time`, and tolerance all hold), taking finalized deposits before transfers
seen above `finalized`; otherwise the first payment. Its `status` is `"seen"` while above
`finalized` (with `confirmations` and `estimated_final_at`) and `"finalized"` once it is a deposit
(then `deposit_id` locates it); `supported`, `in_time`, and `amount_within_tolerance` describe it
against the lock. On a cancelled lock every payment is valued at spot, so `in_time` and
`amount_within_tolerance` are false; the lock's own `status` shows why. An expired lock still
applies to a payment mined before `expires_at` (§9), so those fields are computed normally there.
`pending-deposits` lists the same view for the account's persistent addresses, separate from
`deposits` so it is never mistaken for a credit. `estimated_final_at` is `block_time + 15
minutes`, the typical Ethereum delay to `finalized` (64 to 95 slots); it is an estimate. A seen
transfer can disappear in a reorg; only `deposits` and `deposit.credited` reflect credit. The
pending view ignores pause scopes: it is informational, and a pause still stops whatever it
stops for the deposit once final. While a chain is frozen (§13) its pending view stops updating.

Events (Standard Webhooks, signed with the settlement key): `deposit.pending`,
`deposit.confirmed`, `deposit.credited`, `deposit.rejected`, `deposit.refunded`,
`rate_lock.expired`. Events never change balances. `deposit.pending` is sent at most once per chain
event when the head scan first sees a transfer to a watched address; its payload (`product_id`,
`external_id`, `deposit_id`, `chain_id`, `tx_hash`, `log_index`, `block_number`, `address`,
`product_lock_ref`, `asset_contract`, `from_address`, `amount_atomic`) is marked
`provisional: true`: the transfer may still be reorged away, and no deposit exists yet. Only
non-zero transfers of routed tokens produce it. The outbox does not order events, so
`deposit.pending` can arrive after `deposit.confirmed` or `deposit.credited` for the same
deposit; receivers must act on state (the deposit or lock they fetch), never on event order.
Every `deposit.credited` and `deposit.rejected` payload carries `product_id`,
`deposit_id`, `chain_id`, `state` (`credited` or `rejected`), and `route` (null when no route
was selected); `deposit.credited` adds the destination transaction and pricing fields, and
`deposit.rejected` adds `reason` (plus `product_reason` for a product refusal). Payload changes
are additive; receivers must ignore unknown fields. OpenAPI from `utoipa`; SDKs generated from
it, shipped with a runnable
Python integration example, a signing helper, and a versioning and deprecation policy. A
sandbox (Sepolia, test token, product credentials, scripted late/under/over/rejected
scenarios) is available to integrators before mainnet.

### Customer experience obligations (product UI)

These follow exchange and payment-processor conventions and are part of the integration
checklist:

| Topic | Requirement |
|---|---|
| Default flow | Quote first: amount input → locked price, exact token amount, single-use address, QR as an EIP-681 URI carrying token and amount, countdown to `expires_at`, and the rule for late or wrong-amount payments. |
| Advanced flow | Persistent address behind an explicit "send any amount" option, with "valued at the rate when the deposit is final" and an indicative current rate. |
| Warnings | Network name and chain id, full token contract, "only PHA on Ethereum", minimum deposit, and that below-minimum deposits are not credited. Never truncate addresses or hashes. |
| Waiting | After payment the user sees "received, N confirmations, final around hh:mm" from the lock's `payment` (`status: "seen"`) or `pending-deposits`, with a transaction-hash lookup and an explorer link; it is not credited and may still disappear in a reorg. Deposits are reported only once final. |
| History | Each deposit shows token amount, rate, valuation time, USD credited, transaction hash, status, and lock reference. |
| Quote page | Shows spread, that network and exchange withdrawal fees are the user's, the lock window, remaining limits, workspace name, promotion eligibility, and what happens on underpayment, overpayment, or late payment. Supports cancel and re-quote; the page is resumable by `lock_ref`. |
| Underpayment | Shows the amount received, the shortfall, and a "top up the difference" re-quote; multiple payments are not accumulated against one lock. |
| Exceptions | Wrong asset or below minimum: "contact support"; the funds are held (§15) and finance may return them per the refund policy. Overpayment beyond tolerance is not an exception: it is credited at spot for the full amount (§9) and not refunded. Sanctions or product refusal: "under compliance review, contact support"; the reason code stays server-side. Paused: "deposits temporarily unavailable", address hidden. |
| After credit | Shows the new available balance, debt settled, and whether service resumed. |
| Notifications | Email or in-app notice on `deposit.credited`, `deposit.rejected`, `deposit.refunded`, and `rate_lock.expired`; optionally "payment received, waiting for finality" on `deposit.pending`. |
| Support | Support staff can look up by transaction hash, address, lock reference, workspace, or order and see the full timeline; every case has an owner and a response target. |

### Deposit status for exchange users (product UI)

Exchange users expect one progress line per deposit. The product maps service states to these UI
states. The service reports a deposit only once it is final (§8); the first state comes from its
display-only pending view (§12): the lock's `payment` with `status: "seen"`, or an item of
`pending-deposits`. That view is not a credit and can disappear in a reorg, and only routed tokens
appear in it; other tokens first show as `rejected(unsupported_asset)` once final. Drive the UI
from fetched state, never from webhook order.

| UI state | Service state | Copy |
|---|---|---|
| Detected, N confirmations | none yet: lock `payment.status` `seen`, or a `pending-deposits` item (display only) | "Payment detected: N confirmations. Final around {`estimated_final_at`}." When `in_time` or `amount_within_tolerance` is false on a lock, add: "This payment does not match the quote, so it will be credited at the rate when it becomes final." |
| Finalizing | `detected` | "Final on Ethereum. Checking the payment and fixing the rate." |
| Crediting | `confirmed`, `cleared` | "Crediting your balance." |
| Completed | `credited`, `swept` | "Credited $X at $rate." When a lock-address payment was valued at spot (late, wrong amount, second payment), add: "Credited at the rate when your payment became final because it did not match the quote." |
| Needs attention | `rejected` | By reason, below. The reason code itself is never shown. |

| `reason` | "Needs attention" copy |
|---|---|
| `unsupported_asset` | "This token is not accepted here, so it was not credited. Contact support to have it returned." |
| `below_minimum` | "This payment is below the minimum deposit of X, so it was not credited. Contact support; amounts at or above the refund minimum can be returned." |
| `out_of_bounds`, `out_of_range` | "This payment is outside the deposit limits, so it was not credited. Contact support to have it returned." |
| `sanctioned`, `product_refused` | "This payment is under compliance review. Contact support." |

### Quote and address copy (product UI)

- Quote page, next to the single-use address: "Paying from an exchange? Use your persistent
  address; exchanges may hold new withdrawal addresses and deduct withdrawal fees, so the amount
  received must still equal the quote."
- QR codes: the persistent address QR encodes the plain address only. Only a quote's QR is an
  EIP-681 URI (token and amount), always shown with copy-address and copy-amount buttons for
  wallets and exchanges that do not read the URI.
- When a quote's `remaining_seconds` reaches 0, hide its QR code and address and show "Payment
  window closed, awaiting finality. A payment sent in time is still credited at the quoted
  price." Offer a re-quote; the lock stays `open` until chain-time expiry (§9).
- Network warning on every address: "Ethereum mainnet only. Payments sent on any other network
  are not credited." Support handles such a payment with the
  [wrong-network deposit runbook](../deploy/runbooks/wrong-network-deposit.md).

## 13. Reconciliation

The reconciler runs every 10 minutes and stores each finding once; repairs are silent, every
other finding raises `TopupReconciliationMismatch` (§16).

| Check | Action |
|---|---|
| Finalized transfer to our address with no deposit row, in the range the scanner has committed | insert `detected` |
| Settlement `sent` with no answer, any age | `GET` by key, adopt the answer |
| `credit_minor` ≠ recomputation from stored inputs | alert, block flush |
| Deposit with no `flush_id` but a confirmed `flushed` row at a later log position | link it (replay of stored events) |
| Address balance ≠ Σ deposits − Σ `flushed.amount_atomic`; treasury inflow from our forwarders ≠ Σ `Flushed` events | alert |
| `addressOf(salt)` on chain ≠ stored address | freeze chain, alert |
| After a restore, in the read-only restore-check instance (§14): every deposit in `cleared`, `credited`, or `swept`, or rejected by the product after `cleared` | `GET` each key and adopt the answer (product wins, §11); resume only when every lookup is complete |

## 14. Configuration and deployment

One route file per chain and asset pair, with its chain settings inline, in the compose, hence
attested: chain and its finality rule, RPC provider ids, factory and implementation addresses,
treasury, token, unit decimals, settlement URL, product key id, and every threshold and spread.
Changing a value is a new version and compose hash; deposits keep the version that created them.
The route is the only source of a product's settlement URL and of the key id its requests are
verified against; the database stores only the product's slug, webhook URL, and public key, and
every loaded route that names one product must agree on both values or startup fails. Bumping
`operator_key_version` is such a new version; bump it only after the admin Safe has granted the
new operator address (§15 Rotation). Pause flags are the only runtime-mutable state. Every
other setting (RPC URLs, admin key, object storage location, public origin, Sentry environment)
is rendered into the compose, so it is attested too. The only dstack encrypted environment
variables are the object-storage credentials and the Sentry DSN. The in-CVM database's passwords
are the hex of `get_key("db/owner/v1")` and `get_key("db/app/v1")`, handed to PostgreSQL and its
clients as tmpfs files (`POSTGRES_PASSWORD_FILE`, `PGPASSFILE`), identical on every CVM of the app
id. Startup refuses to run without the dstack
socket, two RPC providers, or the on-chain contract checks of §4.

All enabled versions are loaded at startup. The highest enabled version of a route is current for
new API operations, while older versions remain available for historical deposits. A chain is
scanned only while it has a loaded route, and its rate locks expire only by its scanner's cursor
(§9), so a route version or a chain's last route is removed only after its open locks and
in-flight deposits have resolved (`deploy/runbooks/route-retirement.md`).

The route's attested `chain.flush` settings own the flush policy: the planning cron,
`max_gas_ratio_bps`, native gas-price asset id, maximum EIP-1559 fee, replacement fee bump, and
the operator gas reserve `min_operator_balance_wei` (§16).
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
  topup:    { image: ghcr.io/phala-network/crypto-topup@sha256:…, command: ["topup", "run"] }
  postgres: { image: ghcr.io/phala-network/postgres-walg@sha256:…,     # postgres:18 + WAL-G
              volumes: [pgdata:/var/lib/postgresql] }   # archive_timeout=60, archive_command=walg-cron wal-push %p
  backup:   { image: ghcr.io/phala-network/postgres-walg@sha256:…, command: ["walg-cron", "backup-push", "0 3 * * *"] }
```

`TOPUP_PUBLIC_ORIGIN` is the service's public scheme and authority behind the gateway (for
example `https://<app-id>-8080.<gateway-domain>`, no path); `topup run` refuses to start without a
valid value.

Postgres on the CVM's encrypted disk; WAL-G daily base backups and continuous WAL with
`archive_timeout=60` and a one-row heartbeat a minute, encrypted with `get_key("backup/v1")`
before leaving the CVM, one key per backup prefix (RPO ≤ 1 min, RTO ≤ 1 h). Restore is a
bootstrap from backup: a new instance of the same app boots the attested restore-check variant of
the compose, whose PostgreSQL restores and promotes with archiving off, whose `topup` is
read-only on its own port, and which runs the post-restore check (§13) and reports it on
`/healthz`; resume upgrades that instance to the service compose (`deploy/RESTORE.md`). The
restore drill runs weekly in CI on a local stack; the staging drill restores staging's real
backups. Addresses need no restore because salts derive from product data. Ingress via the
dstack gateway; egress limited to providers, price sources, object storage, product URLs, and
Sentry. The CVM runs the non-dev OS image `dstack-0.5.9`, the latest dstack release a Phala
Cloud node offers; deploy preflight refuses any other image and a node set that does not offer
it. Upgrade = reproducible build → digest (Release images on `main`) → compose hash → CI
deploy (the dispatcher is accountable; no approval gate) → attested read-back. Keys come from Phala Cloud's
KMS, with no on-chain compose-hash allow-list: funds go only to the immutable treasury and the
product verifies every settlement on its own node, so a malicious upgrade could cause downtime or
read service data but not move funds or credits, and the attested compose hash makes it
detectable.
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
| Addresses | One persistent address per (account, chain), reusable forever; `rotate` creates version + 1 and keeps the old one valid and monitored. Lock addresses are single-use. |
| Dust and mistakes | Below-minimum and unsupported-asset deposits are recorded, visible, and not credited. Rejected deposits of a routed token are flushed to the treasury with everything else; an unsupported token stays in its forwarder, since the flusher sweeps only routed tokens, until a separately reviewed Safe flush. |
| Refunds | Refundable: wrong token; below the minimum credit but at or above `min_refund_atomic` *(policy)*; rejected for any reason other than sanctions; funds arriving after the workspace closed. Not refundable: credited USD, including an overpayment beyond tolerance (credited at spot for the full amount, §9), and below-minimum dust under `min_refund_atomic`. The user requests a refund with a destination address they control (never defaulted to `from_address`, which may be an exchange hot wallet); finance approves and executes from the treasury Safe; the service records the transaction, emits `deposit.refunded`, and reconciles it. Refunds are in the original token net of gas, within a published processing time. |
| Workspace closure | Unused credit and in-flight deposits follow the product's closure policy; the old address stays monitored, and later funds are held for refund. Late funds are refundable because the product answers `rejected` for a closed workspace, recorded as `rejected(product_refused)` (§11); the service has no closure check of its own. |
| Compliance | Direct sanctions screening from the pilot; region and Travel Rule applicability decided in Phase 0; KYT adapter and a compliance case flow (customer information request, reviewer role, response time, disposition) before GA. Record requests follow a documented verification, approval, and delivery procedure. |
| Fees and exposure | Gas is a service cost; credit is never reduced. Treasury bears price exposure between valuation and flush, and open rate-lock exposure up to the caps. |
| Rotation | Operator key: grant `operator/v2`, revoke `v1` (admin Safe); flush nonces are tracked per operator address, so the new key starts at nonce 0 without conflict. Settlement key: add `settlement/v2`; products accept both for 30 days. Backup key: a new domain and a new prefix; the old prefix is kept until the new one holds a full retention window. |
| Retention | Deposits, transitions, settlements, audit: 7 years *(policy)*, append-only. |
| Kill switches | Pause scopes (`quotes`, `addresses`, `settlement`, `flush`, `refunds`) at account, product, or route level. Each scope's customer-facing effect is documented and shown; pausing never rolls back a credited fact. Incidents are announced on the product status page with affected routes and updates. |
| Runbooks before pilot | operator key compromise, provider disagreement, price outage, stuck settlement, `422` payload mismatch, restore, treasury change, gas refill, refund execution, rejected funds at treasury. |

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
age, unflushed balance, open lock exposure, flush planning, reconciliation) is in the daily admin
report (`GET /v1/admin/report/daily`). Log lines and their spans (`deposit_id`, `chain_id`,
`state`, `attempt`) serve local stacks.

Tests. `core`: exhaustive transitions, `proptest` on credit math, CREATE2 math against
Foundry, route schema. Contracts: Foundry unit, fuzz, and invariant tests (`flush` can only
pay the treasury; clone address prediction; ETH path; reentrancy with a hook token).
Integration on `anvil` + Postgres: happy path; duplicate logs; provisional evidence corrected
after provider agreement; racing pumps; stale lease; crash between intent and send; stale or
divergent prices; sanctions hit; settlement `processing`, `409`, `422`, `rejected`, timeout
then `GET`; lock exact, over, under, late, double payment; batch flush with replacement,
reverted flush, and operator rotation; flush carrying pending and rejected deposits; deposit
backfilled after its flush; deposit arriving while a flush is unconfirmed; restore from a
pre-settlement snapshot with `GET`-first adoption. Conformance suite against the reference
product in CI (`make product-conformance`), including obligations 4 and 5; `signer::dstack`
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
| Quote-first checkout: spread and fee disclosure, exact amount, EIP-681 QR, countdown, resume by `lock_ref`, cancel, re-quote | S+P | ✓ | | |
| Underpayment shortfall and top-up re-quote; overpayment beyond tolerance credited at spot | S+P | ✓ | | |
| Persistent address as advanced option with indicative rate | S+P | ✓ | | |
| Wallet and exchange payment guidance, mobile deep link, copy fallback | P | ✓ | | |
| Waiting screen with distinct stages, last update, when to ask for help | P | ✓ | | |
| Pre-finality "seen" payment view and `deposit.pending` notification | S | ✓ | | |
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
| Webhook delivery log, test send, replay | S | CLI | ✓ | |
| Multi-product tenancy administration; self-serve product onboarding | S | | | ✓ |
| Sender address book and source whitelisting | S+P | | | ✓ |
| Built-in token purchase, withdrawal, trading account | — | | | never |
