# Crypto Top-up Service Architecture

## 1. Purpose

Build a reusable service that converts finalized crypto deposits into credits in a destination product. The service must preserve an auditable valuation, produce exactly one business mutation per chain event, and complete routine processing without human approval.

The initial route is Ethereum Mainnet PHA to Phala Cloud USD credit. Product and asset details enter through route configuration and adapters.

## 2. Business invariant

For every supported canonical deposit event:

```text
one chain event
  → at most one deposit record
  → at most one accepted settlement
  → at most one destination ledger mutation
```

A credited amount is derived from persisted source quantity, quote evidence, valuation time, rounding policy, and settlement unit. Replaying any worker preserves the same result.

## 3. Domain model

### Product

A destination system that owns customer balances and consumption. A product exposes an idempotent settlement adapter.

### Account

A product-scoped destination for credits, such as a workspace or customer account. External account IDs are opaque to this service.

### Asset

A chain-native currency or token contract with an immutable chain-specific identifier and decimal precision.

### Route

A versioned configuration connecting a source asset to a product credit unit. A route selects finality, pricing, risk, custody, settlement, and sweep policies.

### Deposit address

A custody-controlled address assigned to an account and route. An address lifecycle is versioned and auditable.

### Deposit

An immutable chain event plus derived decisions and processing state.

### Quote

The stored market observations and deterministic calculation used to derive the settlement amount.

### Settlement

An idempotent request to mutate the destination product ledger and the destination receipt proving the mutation.

### Sweep

A custody transaction that moves deposited assets to treasury after settlement eligibility and economic threshold checks.

## 4. Adapter boundaries

### Chain adapter

Responsibilities:

- subscribe to low-latency chain events
- scan historical ranges with durable cursors
- normalize events into canonical deposit candidates
- fetch block and transaction evidence
- report finalized and canonical status
- track replacement or reorganization evidence

Initial implementation: Ethereum JSON-RPC and ERC-20 `Transfer` logs.

### Custody adapter

Responsibilities:

- derive or provision deposit addresses
- return opaque key references
- sign gas-funding and sweep transactions
- expose transaction state

Key material never enters service storage or logs.

### Risk adapter

Responsibilities:

- screen source address and transaction evidence
- apply sanctions, classification, amount, velocity, and route policies
- return `cleared`, `denied`, or retryable `inconclusive`
- persist provider version and evidence references

### Pricing adapter

Responsibilities:

- query independent market sources
- normalize observations into a requested pair
- enforce freshness and deviation policy
- return a deterministic quote with source evidence

### Settlement adapter

Responsibilities:

- submit an idempotent credit request to a destination product
- return a stable destination transaction ID and receipt
- expose settlement status for recovery and reconciliation

The adapter contract must preserve the service deposit ID as the destination idempotency key.

## 5. Automated lifecycle

```text
address_requested
  → address_active
  → detected
  → confirming
  → risk_checking
  → pricing
  → settlement_pending
  → credited
  → sweep_pending
  → swept
```

Retryable side states:

- `pending_chain`: RPC evidence is incomplete or inconsistent
- `risk_hold`: the risk result is temporarily inconclusive
- `pending_price`: market observations are unavailable, stale, or divergent
- `settlement_retry`: the destination result is unknown or retryable
- `sweep_retry`: gas funding, signing, broadcast, or confirmation is retryable
- `failed`: retry budget exhausted or an invariant failed

Terminal policy state:

- `rejected`: a deterministic policy denial; destination balance remains unchanged

Every transition stores its input evidence, policy/configuration version, worker attempt, timestamp, and output.

## 6. Event identity and idempotency

For EVM log-based deposits:

```text
chain_id + asset_contract + transaction_hash + log_index
```

Required uniqueness constraints:

- normalized chain event identity
- active account/route deposit address
- deposit to settlement intent
- settlement destination transaction ID
- sweep transaction identity

Workers claim transitions using database locking or compare-and-set versioning. External effects use stable idempotency keys and an outbox/inbox pattern.

## 7. Finality

A route chooses a finality policy exposed by its chain adapter. For Ethereum, settlement eligibility requires:

- the event block is at or below the RPC `finalized` head
- the stored block hash matches the canonical block at that height
- the token contract, destination address, and decoded amount match persisted evidence

The scanner backfills ranges with `eth_getLogs`; WebSocket events only reduce detection latency.

## 8. Valuation

A route defines:

- source asset and destination unit
- primary and independent quote sources
- maximum observation age
- maximum source deviation
- rounding mode and precision
- minimum creditable amount

The initial valuation point is the first persisted observation that the deposit became finalized:

```text
valuation_at = first_finalized_observation_at
credit_amount = round_down(source_amount × validated_price, destination_precision)
```

Retries retain `valuation_at`. The quote record stores raw observations, normalized prices, timestamps, selected price, calculation inputs, and policy version.

## 9. Settlement contract

The service sends a signed or mutually authenticated request equivalent to:

```json
{
  "idempotency_key": "deposit:<deposit-id>",
  "account_id": "<product-account-id>",
  "unit": "USD",
  "amount": "12.34",
  "source": "crypto_deposit",
  "evidence_uri": "<immutable-receipt-reference>"
}
```

A successful response contains a stable destination transaction ID. Timeouts remain `settlement_retry` until the adapter queries the destination by idempotency key. The service never creates a second settlement to resolve an unknown response.

## 10. Sweep lifecycle

A route defines a treasury destination and economic sweep threshold. The automated worker:

1. calculates the finalized unswept balance
2. estimates transfer gas and economic viability
3. requests minimal gas funding when needed
4. requests an MPC/HSM signature
5. broadcasts the asset transfer
6. tracks replacement, confirmation, and treasury receipt
7. updates the sweep allocation for included deposits

Nonce allocation, fee replacement, and gas funding use idempotent custody requests.

## 11. Reconciliation

Continuous and scheduled reconciliation compares:

- chain transfers to registered addresses
- deposit records and state transitions
- quote and risk evidence
- settlement intents and destination ledger receipts
- address balances
- gas-funding and sweep transactions
- treasury receipts

Safe missing transitions are repaired automatically. Invariant violations emit alerts and preserve all evidence for debugging.

## 12. API surface

Initial control-plane API:

```text
POST /v1/accounts/{account_id}/deposit-addresses
GET  /v1/accounts/{account_id}/deposit-addresses
GET  /v1/deposits/{deposit_id}
GET  /v1/accounts/{account_id}/deposits
POST /v1/routes/{route_id}/pause
POST /v1/routes/{route_id}/resume
```

Product events:

```text
deposit.detected
deposit.confirming
deposit.credited
deposit.rejected
deposit.failed
sweep.completed
```

Events include a stable event ID and support at-least-once delivery. Consumers deduplicate by event ID.

## 13. Data storage

Minimum tables:

- `products`
- `accounts`
- `assets`
- `routes`
- `deposit_addresses`
- `chain_cursors`
- `deposits`
- `risk_decisions`
- `quotes`
- `settlements`
- `sweeps`
- `outbox_events`

Financial quantities use fixed-precision decimal or integer atomic units. Every record carries route and policy versions.

## 14. Security

- Keep private keys in custody/MPC/HSM systems.
- Authenticate product and provider integrations with scoped identities.
- Sign outbound settlement and webhook events.
- Encrypt sensitive provider evidence at rest.
- Redact RPC, custody, risk, and pricing credentials from logs.
- Restrict route changes, treasury destinations, and pause controls through audited authorization.
- Apply request replay protection and idempotency to every external mutation.

## 15. Observability

Required metrics:

- scanner lag and cursor height
- deposits by route and state
- finality latency
- risk and pricing latency/outcomes
- quote source age and deviation
- settlement latency and retries
- unswept balance and sweep cost
- reconciliation mismatches

Every deposit has a traceable timeline keyed by service deposit ID and chain event identity.

## 16. First route profile

The first route validates the generic contracts with:

- chain: Ethereum Mainnet
- asset: PHA ERC-20
- destination product: Phala Cloud
- destination unit: USD
- settlement: existing Phala Cloud top-up/order completion path
- custody: MPC/HSM-backed workspace address
- treasury: configured PHA treasury

Product-specific billing behavior remains behind the settlement adapter.

## 17. Acceptance criteria

- An account receives a deposit address through the API.
- A supported transfer is recovered after worker or RPC interruption.
- Finalized, risk-cleared, and validly priced deposits settle automatically.
- Duplicate events and concurrent workers produce one destination ledger mutation.
- Risk, pricing, RPC, settlement, and custody outages resume from persisted states.
- Eligible balances sweep to treasury automatically.
- Reconciliation detects and repairs safe missing transitions.
- A second route can be added through configuration plus adapters, without changing the core deposit state machine.
