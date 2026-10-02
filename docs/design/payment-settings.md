# Design: per-account payment settings

Status: **proposed** (2026-10-02), for review before implementation. Ships in 0.6.0, breaking,
with no backward compatibility. When accepted, it amends [multi-tenant design](multi-tenant.md)
D1, D16, §12, §14, and §15, and the [architecture](../architecture.md) §6–§9, §12, and §14.

## 1. Problem

The attested route file mixes two different things: the operator's **catalog**, meaning what the
instance can safely accept (contracts, RPCs, pricing sources and guards), and **commercial policy**,
which belongs to each merchant (which tokens it takes, its quote terms, its minimums and maximums).
Per-account policy is also scattered and partly implicit:

- A chain is enabled for an account implicitly, by having a treasury there.
- Every routed asset on that chain is accepted. A merchant cannot ask for "stablecoins only".
- `confirmation_policies` is a separate table, per account rather than per mode, set through
  `POST /v1/account`.
- Quote terms (`window_s`, `spread_bps`, `tolerance_bps`, `amount_decimals`), deposit bounds,
  `min_credit_minor`, and `min_refund_atomic` are the same for every merchant on the instance.
- Quotes, deposit addresses, `GET /v1/config`, valuation, screening, and refunds each re-derive
  these rules from the route.
- A quote does not keep its terms. Its tolerance and `GET /v1/quotes/{id}`'s asset are read from
  the route's *current* version (`api/quotes.rs` `quote_object`, `core/valuation.rs`).
- `quote.max_creations_per_minute` is set per route but enforced per customer across every route
  (`locks::check_creation_rate` counts all of the customer's quotes), so its value depends on
  which route the next quote happens to use.

## 2. Decision

1. **Two layers.** The attested config is the operator's **catalog**: what can be accepted,
   how it is valued and screened, and, for each merchant parameter, a **default and hard bounds**.
   Each account has, **per mode**, one **payment settings** object that chooses from the catalog:
   the chains and assets it accepts, and its commercial terms within the bounds.
2. **Opt-in, explicit.** An account with no settings in a mode accepts nothing in that mode. No
   implicit "every routed asset", and no chain enabled by a treasury alone.
3. **Treasury proof stays its own security flow** (challenge, signature, time-lock, cancellation;
   D10). A chain is *active* for issuance only when it has both an accepted asset in the settings
   and an active treasury.
4. **One computation.** `effective_config(catalog, settings, treasuries)` resolves everything, and
   every consumer reads only its result (§6).
5. **Deposits are held to the settings in force when they are recorded**, or, for a payment of a
   quote, to the quote's own terms (§8). A deposit the governing settings do not accept is
   `rejected` with `asset_not_accepted`, never credited, and refundable.

## 3. Field classification

Rule: a field is **operator** if it describes the chain, the token, the evidence the service
trusts, or the service's own cost or safety. It is **merchant** if it is a commercial choice whose
consequence the merchant bears (D14: the merchant, not the operator, bears price exposure and
pays refunds). A merchant field gets an operator default and hard bounds wherever an extreme value
would harm payers, the service, or the evidence.

### Route fields

| Field (today) | Owner | Reasoning |
|---|---|---|
| `route`, `version`, `livemode` | Operator (catalog identity) | Attested identity; a quote and a deposit record the version that valued them. |
| `chain.chain_id`, `forwarder_factory`, `implementation` | Operator | Contracts: the custody model (D2, D3). |
| `chain.rpc_providers` | Operator | Evidence sources and their keys. |
| `chain.sanctions_oracle` | Operator | Screening evidence (compliance is the operator's, 2026-10-01). |
| `chain.confirmations` | Operator **floor**; merchant may be stricter | The floor is the operator's reorg-risk judgment (D1); the merchant may require more, never less (today's `confirmation_policies`). The floor is also the default. |
| `asset.symbol`, `contract`, `decimals` | Operator | Token identity; credit arithmetic depends on `decimals`. |
| `asset.backstop` | Operator | Scanner strategy and RPC cost. |
| `pricing.mode` (`spot`/`stablecoin`) | Operator | A property of the asset's valuation. The service never infers it. |
| `pricing.primary`, `check`, `fx` | Operator | Price evidence sources. |
| `pricing.max_age_s`, `max_deviation_bps`, `max_fx_deviation_bps` (depeg guard) | Operator | Guards on the evidence. A merchant loosening them would weaken what the attestation vouches for. |
| `alerts.stuck_after_s.*` | Operator | Operations. |
| `quote.window_s` | Merchant, bounded (`min`, `max`) | The merchant bears the price exposure of an open quote. The `max` bounds how stale an attested locked price may get and how long a quote holds the account's open-quote caps; the `min` keeps a quote payable. |
| `quote.spread_bps` | Merchant, bounded (`max`) | The merchant's pricing. The `max` protects payers from an attested price marked down without limit. |
| `quote.tolerance_bps` | Merchant, bounded (`max`) | How much underpayment the merchant forgives at the locked credit. The `max` keeps "complete the quote" meaningful. |
| `quote.amount_decimals` | Merchant, bounded by `asset.decimals` | A display choice for payers: the rounding-up overpayment (under one unit of the last decimal) is the payer's, and the credit is unchanged. No operator bound beyond the token's decimals. |
| `quote.max_creations_per_minute` | Merchant, **per account and mode** (not per route), bounded (`max`) | It limits one customer's quotes so that customer cannot exhaust the merchant's open-quote caps, which is a merchant concern. The `max` protects the service's pricing and API budget. It is already enforced per customer across routes, so it moves to the account level. |
| `limits.min_credit_minor` | Merchant, bounded (`min`) | The smallest sale the merchant makes. The `min` is the operator's dust floor: each deposit costs RPC reads and a webhook. |
| `limits.min_deposit_atomic` | Merchant, bounded (`min`) | As above, in token units. |
| `limits.max_deposit_atomic` | Merchant, bounded (`max`) | The `max` is the operator's risk bound: valuation at spot is only meaningful within market depth, and screening is per deposit. The merchant may set a lower value. |
| `limits.min_refund_atomic` | Merchant, bounded (`min`) | The merchant pays refunds from its own treasury and gas (D5, D14), so the dust floor is its call. The operator's `min` (default 0) only stops refund spam the service would have to verify. |

### Account fields

| Field | Owner | Reasoning |
|---|---|---|
| `name`, `contact`, `due_diligence`, `restricted` | Operator | Onboarding record (D8). |
| `charges_enabled` | Operator | Go-live gate (D12). |
| `max_unfinalized_credit` | Operator | Bounds the service's reorg exposure (§7). |
| `account_limits` (open quotes, open credit per account and per customer, active deposit addresses) | Operator | Resource and abuse caps on the instance (§12). Unchanged. A merchant-side lower cap can be added to the settings later. |
| `paused_scopes` (operator), route pauses, customer pauses | Operator | Incident controls. |
| `self_paused_scopes` (`quotes`), treasury crediting pause | Merchant, but **not settings** | Emergency switches that must act at once and alone (§12). They stay their own endpoints. |
| `confirmation_policies` | Merchant → **moved into payment settings**, per mode | Removed as a table and as a field. |
| Treasuries | Merchant, own security flow | Unchanged (D10). Settings never set or imply a treasury. |
| Webhook keys and endpoints | Merchant, own resources | Unchanged. |

## 4. API: `GET` and `POST /v1/payment_settings`

**Shape.** This is a singleton resource per account and mode, read and written with the key's
mode, like Stripe's singleton settings resources [Tax Settings](https://docs.stripe.com/api/tax/settings)
(`GET`/`POST /v1/tax/settings`, object `tax.settings`, event `tax.settings.updated`) and
[Balance Settings](https://docs.stripe.com/api/balance-settings). The catalog view follows
Stripe's [payment method configurations](https://docs.stripe.com/api/payment_method_configurations/object),
which list each method with whether it is `available` and the account's preference.

**Why not a field on the account.** The settings are a large nested document with their own
validation paths (`chains[0][assets][1][quote_spread_bps]`), their own version history (§8), and
their own change event. On the account, every `account.updated` would carry the whole document,
operator and merchant changes would share one event, and `GET /v1/account` would grow with the
catalog. A singleton keeps the account object about the operator's decisions, and gives the
settings a clean object, event, and diff.

```http
GET /v1/payment_settings
```

```json
{
  "object": "payment_settings",
  "livemode": false,
  "version": 4,
  "updated": 1790000000,
  "quote_creations_per_minute": null,
  "chains": [
    {
      "chain_id": 84532,
      "confirmations": null,
      "assets": [
        {"asset": "usdc", "quote_spread_bps": 0, "quote_ttl_seconds": null, "...": null},
        {"asset": "usdt"}
      ]
    }
  ],
  "available": [
    {
      "chain_id": 84532,
      "status": "active",
      "confirmations": {"floor": "3", "default": "3"},
      "assets": [
        {
          "asset": "usdc", "contract": "0x…", "decimals": 6, "pricing": "stablecoin",
          "accepted": true,
          "quote_ttl_seconds": {"default": 900, "min": 60, "max": 3600},
          "quote_spread_bps": {"default": 0, "max": 500},
          "...": {}
        }
      ]
    }
  ]
}
```

- `chains` holds what the merchant set. `null` means the operator's default, so the merchant
  follows a change of the default. A chain or asset that is not listed is not accepted.
- `available` is read-only: the catalog of the key's mode with defaults, bounds, `accepted`, and
  each chain's `status` (`active`, `treasury_not_set`, or `not_configured`), so a merchant can see
  what it may choose without asking.
- The parameter names match `GET /v1/config`'s (`confirmations`, `min_amount`,
  `min_deposit_atomic`, `max_deposit_atomic`, `min_refund_atomic`, `quote_ttl_seconds`,
  `quote_spread_bps`, `quote_tolerance_bps`, `quote_amount_decimals`), so one name means one thing
  in the settings and in the effective config.

```http
POST /v1/payment_settings
{"chains": [{"chain_id": 84532, "assets": [{"asset": "usdc", "quote_spread_bps": 0}, {"asset": "usdt"}]}]}
```

- **Semantics.** A parameter that is not sent is unchanged; `quote_creations_per_minute: null`
  restores the default. `chains`, when sent, **replaces** the whole list, as Stripe replaces an
  array parameter: declarative and atomic, with no merge rules. `chains: []` accepts nothing.
- **Validation** against the catalog of the key's mode, returning `400 parameter_invalid` with
  `param` naming the exact path:
  - an unknown chain;
  - an asset not routed on that chain (the message lists the routed ones);
  - a chain or asset listed twice;
  - a value outside its bounds (the message states the bounds);
  - `confirmations` weaker than the floor or of the wrong chain family (today's rules);
  - `quote_amount_decimals` above `decimals`;
  - `min_deposit_atomic` above `max_deposit_atomic`.
- **Response.** The new object, with `Idempotency-Key` honored as on every `POST`.
- **Event.** Each change writes a new version (§8), an audit row (`payment_settings.update`), and
  `payment_settings.updated` with `previous_attributes`. It is an account event, delivered to every
  enabled endpoint whatever it subscribes to, as `account.updated` is. A request that changes
  nothing writes nothing.

**Permissions.** Reading needs `account.read`. Writing needs `account.write`, which a restricted
key can never hold (`tenancy::Principal`), so only a secret key changes what an account accepts.
No new permission: a restricted production key (the reference product's included) reads the
settings and `GET /v1/config` but cannot widen what the account accepts. This matters because the
setting decides which tokens are credited.

**Errors.**

- `POST /v1/quotes` for a pair the account does not accept: `400 asset_not_accepted` (a new
  code), `param: asset`.
- `POST /v1/deposit_addresses` when no chain is active: `400 asset_not_accepted` if nothing is
  accepted in the mode, or `treasury_not_set` if the accepted chains have no treasury.
- A quote on a configured chain without a treasury: `400 treasury_not_set`, as today.

**`GET /v1/config`** returns exactly the effective config of the key's scope (§6): only active
`(chain, asset)` pairs, each with its resolved terms, effective `confirmations`, and
`typical_credit_seconds`, plus `quote_creations_per_minute` and the operator's open-quote caps as
today. An account with no settings gets `assets: []`.

## 5. Bounds model

The route file keeps its catalog sections and replaces `quote:` and `limits:` with one `merchant:`
section of defaults and bounds:

```yaml
merchant:                         # each account's terms on this route: default, and hard bounds
  quote_ttl_seconds:  { default: 900, min: 60, max: 3600 }
  quote_spread_bps:   { default: 50, max: 500 }
  quote_tolerance_bps: { default: 100, max: 500 }
  quote_amount_decimals: { default: 4 }                    # at most asset.decimals
  min_amount:         { default: 100, min: 1 }             # cents
  min_deposit_atomic: { default: "0" }
  max_deposit_atomic: { default: "200000000000000000000000", max: "200000000000000000000000" }
  min_refund_atomic:  { default: "20000000000000000000", min: "0" }
```

- **Chain floor.** `chain.confirmations` is the floor and the default. Every route of a chain
  must agree, as today.
- **Account-level parameters** that are not per route (`quote_creations_per_minute`) get their
  default and `max` in the service config file, beside `routes`.
- **Code defaults.** Every bound omitted from the file has a code default, printed by
  `topup config show` and `topup route show`, as every route default is today. `topup config
  check` refuses a default outside its own bounds.
- **Tightened bounds.** Bounds are checked when settings are written. If a later route version
  tightens a bound below a stored value, the effective config clamps the value to the new bound,
  `available` shows it, and the next `POST` must comply. The operator never edits a merchant's
  settings.

## 6. The effective config

One module (`crate::payment_config`) holds one pure function and two thin loaders:

```text
resolve(route: &RouteFile, settings: Option<&ChainAndAssetSettings>, account: &AccountSettings) -> Option<Terms>
    -- None if the settings do not accept the route's (chain, asset); else every term resolved:
    -- the merchant's value, or the default, clamped to the bounds; confirmations = stricter(floor, merchant)

effective_config(catalog: &RouteSet, scope, settings: &PaymentSettings, treasuries) -> EffectiveConfig
    -- for issuance: current route versions of the mode, accepted by the settings,
    -- on chains with an active treasury; Terms per pair, typical_credit_seconds per chain

deposit_terms(catalog, deposit) -> Option<Terms>
    -- for a recorded deposit: resolve(the deposit's route version, the settings version
    -- that governs it (§8)), or the quote's stored terms when it pays its quote
```

| Consumer | Reads |
|---|---|
| `GET /v1/config` | `effective_config` |
| `POST /v1/quotes` | `effective_config`; the pair or `asset_not_accepted`; pricing, window, spread, tolerance, decimals, bounds; the resolved `Terms` are stored on the quote |
| Quote views, pending payment, and lock matching | the quote's stored terms (never the current route) |
| Deposit addresses (issuance, networks, listed assets, client view, `typical_credit_seconds`) | `effective_config` |
| Scanner, finality-watch successor, and reconciler inserts | record the governing settings version in the insert statement (§8). No resolution. |
| Confirm step (acceptance, confirmations, valuation, `below_minimum`) | `deposit_terms` |
| Screen step (`out_of_bounds`) | `deposit_terms` |
| Refunds (dust floor `min_refund_atomic`) | `deposit_terms` |
| Admin account view (`GET /v1/admin/accounts/{id}`), admin deposit view | each mode's settings and `effective_config`; a deposit's governing version and terms |
| Restore (re-issued quotes and addresses, settings re-application) | §10 |

Runtime gates (pauses, frozen chains, caps, rate limits) are applied on top of the effective
config, as today. They are state, not configuration.

## 7. Deposits the config does not accept

A transfer of a **routed** token to an issued address is recorded on its route as today. If the
governing settings (§8) do not accept its `(chain, asset)`, whether because the asset is not
listed or the chain is not configured, the confirm step rejects it with
`rejected(asset_not_accepted)` once both providers agree on it, before valuation, in place of the
pricing that `below_minimum` follows. This produces `deposit.rejected`; the deposit is never
credited, and it is refundable through the existing rejected-deposit flow (declare, pay from the
treasury of the deposit's address, `mark_paid`, verified; `core::refund_eligibility`) with the
route's dust floor. A transfer of a token **without a route** remains `unsupported_asset`. The
deposit address page shows only accepted assets, so `asset_not_accepted` normally comes from an
outdated page or a stale address.

## 8. Change semantics: which config governs

- **Versions.** Each change appends an immutable row (`payment_settings`: account, mode,
  `version`, the document, actor, created). The current settings are the highest version.
  Writers lock the account row (`FOR NO KEY UPDATE`), so versions are gapless and concurrent
  writes apply one after the other.
- **A quote keeps the terms it was issued with.** Issuance stores the resolved `Terms` (spread,
  window, tolerance, decimals, confirmations, minimums and bounds) and the settings version on
  the quote. A payment of the quote's asset to the quote's address, on time or late, is governed
  by those terms, even if the merchant later removes the asset. This fixes today's re-reading of
  the current route.
- **Every other deposit** (a deposit address payment, or a quote's address paid in another
  asset) is governed by the settings version **in force when the deposit is recorded**. The
  insert reads the current version in its own statement and stores it as
  `deposits.payment_settings_version`, so the choice is made once and atomically. A change
  applies to every deposit recorded after it commits, and to none recorded before.
- **Confirmations follow the same rule.** Today a stricter policy also applies to deposits
  already recorded and not yet credited. Under this design, a deposit uses the confirmations of
  its governing version. The incident tools for deposits in flight are the merchant's treasury
  crediting pause and the operator's `settlement` pause, which act at once.
- **Deposits recorded before the migration** have no version (`NULL`) and are governed by the
  catalog defaults of their recorded route version, which are exactly the route values they
  were recorded under. Open quotes from before the migration get their `Terms` written by the
  migration from their route's values. This is the only transition rule; nothing else keeps the
  old model.

## 9. Migration and staging

- **Schema.** Migration `…_payment_settings`:
  - create the append-only `payment_settings` table (`topup_app`: `SELECT`, `INSERT`);
  - add `deposits.payment_settings_version` and `quotes.terms` / `quotes.payment_settings_version`
    (open quotes backfilled from their route, §8);
  - add `asset_not_accepted` to the deposit reasons;
  - **drop `confirmation_policies`**.
  - No account gets settings: every account accepts nothing until it is configured.
- **Before the deploy,** the operator records each account's `confirmation_policies` from
  `GET /v1/account`, since the migration drops them. They are re-set in the new settings.
- **After the deploy**, each staging account is configured with its secret test key. The
  reference product account (`acct_3a36c44a…`) accepts every staging route:

  ```sh
  curl -fsS https://pay-api-staging.phala.com/v1/payment_settings \
    -H "Authorization: Bearer $SECRET_TEST_KEY" -H 'content-type: application/json' \
    -d '{"chains": [
          {"chain_id": 11155111, "assets": [{"asset": "pha"}, {"asset": "usdc"}, {"asset": "usdt"}]},
          {"chain_id": 84532, "assets": [{"asset": "pha"}, {"asset": "usdc"}, {"asset": "usdt"}]}]}'
  ```

  The implementation PR's report lists the exact call for each staging account, with the
  confirmations recorded before the deploy. Until an account is configured, its quotes and new
  addresses answer `asset_not_accepted`, and payments to its existing addresses are rejected and
  refundable, so the calls run right after the deploy.
- **Route files** of every environment (staging, the example, and the Phala Cloud template) move
  `quote:` and `limits:` values to `merchant:` defaults with explicit bounds, by pull request,
  as an attested change.
- **Setup docs**: the merchant setup steps ([integration guide](../integration.md)), operator
  onboarding ([deployment reference](../../deploy/README.md#operator-onboarding)),
  [self-hosting](../self-hosting.md), and the [reference product's setup](../../deploy/phala.md#staging-reference-product)
  gain a step after the treasury: `POST /v1/payment_settings` with the secret key. The reference
  product's driver and local stacks (`deploy/local`, the sandbox, the restore drill, the CVM
  rehearsal, and the `product/web` e2e fake service) configure every chain and asset they use.

## 10. Restore

- Settings are rows, restored with the database. Versions written after the restore point are
  lost.
- The merchant has each lost version as a signed `payment_settings.updated` delivery. While
  frozen, the operator re-applies the latest per mode with `POST /v1/admin/restore/payment_settings`
  (the `v1a` signature verified with the account's webhook keys, as `treasuries/apply` does). It
  is audited and not announced again, so the rescan records deposits under the merchant's current
  settings.
- **Delivered outcomes stand.** The confirm step already values a rebuilt deposit at its
  delivered credit before any check. The settings check comes after that branch, so a delivered
  credit is never rejected. A rebuilt deposit whose delivered event is `deposit.rejected` is
  rejected again with the delivered reason, whatever the settings say now, so the merchant never
  sees a rejected deposit later credited.
- A deposit whose outcome was never delivered is governed by the settings version in force when
  the rescan records it.
- Re-issued quotes never apply their merchant-recorded lock (unchanged). They store the `Terms`
  of the settings in force at re-issue. Re-issued deposit addresses are rebuilt on every chain
  they had, as today. Settings never block a re-issue, because the address was given out already.

## 11. What is removed

- `confirmation_policies`: the table, `GET /v1/account`'s field, and `POST /v1/account` (its only
  parameter; the endpoint is removed). The pause, resume, and webhook key roll endpoints stay.
- The implicit model: acceptance of every routed asset, and a chain enabled by a treasury alone.
- Route `quote:` and `limits:` sections, and the per-route `max_creations_per_minute`.
- Every read of merchant terms from a route outside `crate::payment_config`, including quote
  views reading the current route version.
- SDK helpers for `confirmation_policies` (Python `account.update(confirmation_policies=…)`).
  The SDKs gain `payment_settings.retrieve()` and `update()` and the generated types.

## 12. Open questions for review

1. The event name is `payment_settings.updated`, rather than `account.updated` carrying the
   settings. This follows Stripe's singleton settings resources (§4).
2. Confirmations of deposits already recorded are fixed at recording (§8), not tightened by a
   later change, so the outcome is decided once.
3. Merchant-side caps below the operator's `account_limits` are left out for now (§3).
