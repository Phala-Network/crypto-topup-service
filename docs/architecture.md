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

A private service, called by the Phala Cloud billing backend, that gives each product account
a persistent deposit address, turns finalized and screened deposits of configured tokens into
USD credit, and credits the product at most once through a signed HTTP call. Routes may offer
rate-locked deposits.

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
2. **Deposits are born final.** The scanner reads only blocks at or below `finalized`.
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
- Deployed with the deterministic deployment proxy using plain salts, identical init code and
  constructor args on every chain. The treasury is a Safe verified on each chain (deployed,
  same owners and threshold) before a route is enabled.
- Changing the treasury is a new factory and a new route version. The admin Safe only grants
  or revokes `OPERATOR_ROLE`.
- Pilot supports plain ERC-20 with verified behaviour (PHA). Fee-on-transfer or rebasing
  tokens are out of scope; hook-bearing tokens require reentrancy tests before enabling.
- Startup verifies on chain: factory code hash, `implementation()`, `treasury()`, and
  `addressOf(sample salt)` against the route file.

## 5. Stack

Rust stable, `#![forbid(unsafe_code)]`, release `overflow-checks = true`. `tokio`, `axum` +
`utoipa`, `sqlx`, `alloy`, `dstack-sdk` pinned to one guest-API version, `secrecy` +
`zeroize`. `core` denies `arithmetic_side_effects`, `float_arithmetic`, `as_conversions`,
`unwrap_used`. `cargo-deny`, committed lockfile, reproducible distroless image by digest.
Contracts: Solidity with OpenZeppelin, Foundry, one external audit.

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
products      id, slug, settlement_url, webhook_url, pubkey, kid, paused_at
accounts      id, product_id, external_id, paused_at               UNIQUE (product_id, external_id)
addresses     id, account_id, chain_id, kind (persistent|lock), version, lock_ref, salt, address, retired_at
              UNIQUE (chain_id, address)
              UNIQUE (account_id, chain_id) WHERE kind = 'persistent' AND retired_at IS NULL
rate_locks    address_id PK, route, amount_atomic, price_scaled, expires_at, consumed_by (deposit_id) UNIQUE
cursors       chain_id PK, scanned_block
deposits      id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
              address_id, account_id, route, route_version, asset_contract, from_address, amount_atomic,
              state, reason, attempt, next_attempt_at, lease_token, lease_until,
              valuation_at, price_scaled, price_source (spot|lock), credit_minor, quote jsonb,
              flush_id, created_at, updated_at
              UNIQUE (chain_id, tx_hash, log_index)
transitions   id, deposit_id, from_state, to_state, attempt, evidence jsonb, created_at
settlements   deposit_id PK, product_id, key, payload jsonb, status (intent|sent|accepted|rejected),
              destination_tx_id, receipt jsonb, sent_at
              UNIQUE (product_id, destination_tx_id) WHERE destination_tx_id IS NOT NULL
flushes       id, chain_id, token, operator, nonce, tx_hash, block_number,
              status (planned|sent|confirmed|reverted), receipt jsonb
              UNIQUE (chain_id, operator, nonce)
flushed       flush_id, address_id, amount_atomic, block_number, log_index   -- one row per Flushed event
              PRIMARY KEY (flush_id, address_id)
outbox        id, event_type, payload jsonb, next_attempt_at, delivered_at
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
once.

| Step | Does |
|---|---|
| `detected → confirmed` | First, `GET` the product by the deterministic key: an existing answer restores the original payload and business state (`credited` or `rejected`) directly, before any quoting or local decision, so a deposit rebuilt after a restore can never diverge from what the product already did. Then, from both providers: `finalized ≥ block_number`, same block hash, same log. While `detected`, evidence is provisional: if both providers agree on different canonical evidence for the same event identity, the row is corrected. In the same step, fetch the quote (§8) and store `valuation_at`, `price_scaled`, `credit_minor`, `quote`. Below `min_credit_minor` → `rejected(below_minimum)`. |
| `confirmed → cleared` | `isSanctioned(from)` on both providers at a recorded block; `min ≤ amount ≤ max` *(policy)*; account and product not paused. |
| `cleared → credited` | Signed `POST` (§11). Body `rejected` → `rejected(product_refused)`. On an unknown result, `GET` before any resend. |
| `credited → swept` | `flush_id` is set: a confirmed `flushed` row exists for the deposit's address and token at a log position `(block_number, log_index)` greater than the deposit's. Evaluated on flush confirmation and on every deposit insert, so backfilled deposits resolve too. |

## 8. Chain, valuation, screening

**Scanner** per chain: read `finalized` from provider A; fetch `Transfer(*, our addresses)`
from any contract in windows ≤ 2 000 blocks and ≤ 1 000 addresses; insert with
`ON CONFLICT DO NOTHING`; advance the cursor after commit. New addresses backfill from
creation; retired and lock addresses stay in the filter. Native ETH is a balance check at
flush time. Each chain's finality rule is declared in its chain file (Ethereum: `finalized`);
a chain is enabled only after its rule is reviewed. Later option: Helios as one provider.

**Valuation** happens inside the confirm step, so `valuation_at` is the finality observation
and the price is always current at fetch time. Spot: primary Coin Metrics `ReferenceRateUSD`
(1-minute), check Binance `PHAUSDT` × Kraken `USDT/USD`; each observation aged ≤ `max_age`
*(policy)* at fetch; `|primary − check| / primary ≤ max_deviation_bps / 10 000`; FX within
`max_fx_deviation_bps`; the primary is used. Any failure retries the whole step. Stablecoin
routes use fixed `1.0` with the reference rate as a depeg guard.

**Screening** is direct sanctions-list screening plus per-deposit bounds. KYT is a separate
adapter that compliance may require before GA.

## 9. Rate locks (off in pilot)

Invoice model with this service's exception profile:

- `POST …/rate-locks {amount_atomic, product_lock_ref}` returns a single-use address,
  `price_lock = price_spot / (1 + spread)` with `spread = spread_bps / 10 000` *(policy)*, and
  `expires_at = now + window` *(policy)*. Locks count against open-exposure caps per account,
  per product, and global *(policy)*, reserved atomically at creation.
- The lock is consumed by the first deposit to its address whose `block_time ≤ expires_at`,
  `asset` matches, and `|amount − locked| ≤ lock_tolerance_bps` *(policy)*; consumption is a
  single `UPDATE … WHERE consumed_by IS NULL`. That deposit is valued at `price_lock`.
- Any other deposit to a lock address (late, wrong amount, second payment) is valued at spot.
  The product shows these rules to the user before payment.

## 10. Signing and flush

```rust
pub trait Signer {
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx>;   // OPERATOR key, pays gas
    async fn sign_settlement(&self, payload: &[u8]) -> Result<Signature>;
}
```

`signer::dstack` derives `operator/v1` (secp256k1) and `settlement/v1` (ed25519) on demand
and zeroizes them.

**Flusher** on a schedule *(policy)*, per (chain, token): select addresses whose on-chain
balance ≥ `min_flush_atomic` and whose share of batch gas ≤ `max_gas_ratio` of value
*(policy)*; write `flushes(planned)`; send one `factory.flush(salts[], token)` under the
operator nonce lock; replace with a higher fee on the same nonce if needed; confirm at
`finalized`; write one `flushed` row per `Flushed` event in the receipt with its block number
and log index. Deposits are then linked by the rule in §7, whatever their state. Recovery after a
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
6. Recompute `deposit_id = uuid_v5(NS, "{chain_id}:{tx_hash}:{log_index}")` from the
   evidence and require `idempotency_key == "deposit:" + deposit_id`, so one chain event can
   never be credited under a second key.

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
recompute any address without the service. The admin key can only pause and resume; each
call writes `audit`.

```text
POST /v1/products/{p}/accounts
POST /v1/products/{p}/accounts/{ext}/deposit-address           persistent; GET same
POST /v1/products/{p}/accounts/{ext}/deposit-address/rotate    version + 1; old stays valid
POST /v1/products/{p}/accounts/{ext}/rate-locks                 single-use address + locked price
GET  /v1/products/{p}/accounts/{ext}/deposits                   GET /v1/products/{p}/deposits/{id}
POST /v1/products/{p}/accounts/{ext}/pause | resume
GET  /v1/attestation?nonce=…
POST /v1/admin/routes/{r}/pause | resume
```

Events (Standard Webhooks, signed with the settlement key): `deposit.confirmed`,
`deposit.credited`, `deposit.rejected`, `rate_lock.expired`. Events never change balances.
OpenAPI from `utoipa`; SDKs generated from it.

## 13. Reconciliation

| Check | Action |
|---|---|
| Finalized transfer to our address with no deposit row | insert `detected` |
| Settlement `sent` with no answer, any age | `GET` by key, adopt the answer |
| `credit_minor` ≠ recomputation from stored inputs | alert, block flush |
| Deposit with no `flush_id` but a confirmed `flushed` row at a later log position | link it (replay of stored events) |
| Address balance ≠ Σ deposits − Σ `flushed.amount_atomic`; treasury inflow ≠ Σ `Flushed` events | alert |
| `addressOf(salt)` on chain ≠ stored address | freeze chain, alert |
| After a restore: every deposit at or beyond `cleared` | `GET` each key before resuming; product answer wins (§11) |

## 14. Configuration and deployment

One chain file and one route file per pair, in the compose, hence attested: chain and its
finality rule, RPC provider ids, factory and implementation addresses, treasury, token, unit,
settlement URL, product key id, and every threshold and spread. Changing a value is a new
version and compose hash; deposits keep the version that created them. Pause flags are the only
runtime-mutable state. Secrets arrive as dstack encrypted environment variables. Startup
refuses to run without the dstack socket, two RPC providers, or the on-chain contract checks
of §4.

```yaml
services:
  topup:    { image: ghcr.io/phala-network/crypto-topup@sha256:…, command: ["topup", "run"] }
  postgres: { image: ghcr.io/phala-network/postgres-walg@sha256:…,     # postgres:16 + WAL-G
              volumes: [pgdata:/var/lib/postgresql/data],
              command: ["postgres", "-c", "archive_mode=on", "-c", "archive_timeout=60",
                        "-c", "archive_command=wal-g wal-push %p"] }
  backup:   { image: ghcr.io/phala-network/postgres-walg@sha256:…, command: ["walg-cron", "backup-push", "0 3 * * *"] }
```

Postgres on the CVM's encrypted disk; WAL-G daily base backups and continuous WAL with
`archive_timeout=60`, encrypted with `get_key("backup/v1")` before leaving the CVM (RPO ≤ 1
min, RTO ≤ 1 h, weekly restore drill in staging). Restore = restore → post-restore check
(§13) → resume; addresses need no restore because salts derive from product data. Ingress
via the dstack gateway; egress limited to providers, price sources, object storage, product
URLs. Upgrade = reproducible build → digest → compose hash → on-chain allow-list → redeploy.
`GET /v1/attestation?nonce=` returns a TDX quote with `report_data = sha256(nonce ‖
settlement_pubkey)`; verifiers run the dstack verification flow (TCB, measurements, allowed
compose) and pin `(keyid, public key)`.

## 15. Operating policies

| Topic | Rule |
|---|---|
| Addresses | One persistent address per (account, chain), reusable forever; `rotate` creates version + 1 and keeps the old one valid and monitored. Lock addresses are single-use. |
| Dust and mistakes | Below-minimum and unsupported-asset deposits are recorded, visible, not credited, and flushed to the treasury with everything else; finance handles them there. |
| Fees and exposure | Gas is a service cost; credit is never reduced. Treasury bears price exposure between valuation and flush, and open rate-lock exposure up to the caps. |
| Rotation | Operator key: grant `operator/v2`, revoke `v1` (admin Safe); flush nonces are tracked per operator address, so the new key starts at nonce 0 without conflict. Settlement key: add `settlement/v2`; products accept both for 30 days. Backup keys keep prior versions. |
| Retention | Deposits, transitions, settlements, audit: 7 years *(policy)*, append-only. |
| Runbooks before pilot | operator key compromise, provider disagreement, price outage, stuck settlement, `422` payload mismatch, restore, treasury change, gas refill, rejected funds at treasury. |

## 16. Observability and tests

Spans carry `deposit_id`, `chain`, `state`, `attempt`. Metrics: scanner lag, deposits by
state and age, provider disagreements, price deviation, settlement outcomes, outbox backlog,
unflushed balance, operator gas, open lock exposure, backup age, reconciliation mismatches.
Alerts on age in state, any mismatch, scanner lag, backup age > 2 min, stopped loop, gas
reserve, lock exposure near cap.

Tests. `core`: exhaustive transitions, `proptest` on credit math, CREATE2 math against
Foundry, route schema. Contracts: Foundry unit, fuzz, and invariant tests (`flush` can only
pay the treasury; clone address prediction; ETH path; reentrancy with a hook token).
Integration on `anvil` + Postgres: happy path; duplicate logs; provisional evidence corrected
after provider agreement; racing pumps; stale lease; crash between intent and send; stale or
divergent prices; sanctions hit; settlement `processing`, `409`, `422`, `rejected`, timeout
then `GET`; lock exact, over, under, late, double payment; batch flush with replacement,
reverted flush, and operator rotation; flush carrying pending and rejected deposits; deposit
backfilled after its flush; deposit arriving while a flush is unconfirmed; restore from a
pre-settlement snapshot with `GET`-first adoption. Conformance suite against Phala Cloud in CI, including obligations 4 and 5;
`signer::dstack` against the simulator; attestation against a recorded quote.

## 17. Delivery

**Phase 0 (two weeks)**: contract audit and deterministic deployment on Sepolia and mainnet;
finance Safe verified on each chain; two RPC providers; object storage; treasury; policy
numbers.

**Phase 1, capped pilot**: full pipeline including flush, on Sepolia then mainnet with a
per-deposit `max`, product-side caps, and allow-listed accounts; rate locks off. Acceptance:
address issued without an operator and recomputable by the product; deposit recovered after
restart and provider interruption; credit only after two-provider finality; duplicates and
concurrency yield one ledger mutation; outages only delay; balances flush and `Flushed`
events match; reconciliation repairs the two safe cases and alerts on the rest; restore issues
no duplicate; every deposit has a full evidence timeline; runbooks exercised once.

**Phase 2, GA**: bounds raised; rate locks on with exposure caps; KYT adapter if compliance
requires; dashboards.

**Phase 3**: Base PHA and USDC routes through chain and route files; same addresses on both
chains.

Issue #2 is split along Phase 0 and 1; the Phala Cloud endpoint is filed in the monorepo.
