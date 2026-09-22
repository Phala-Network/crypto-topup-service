# Crypto Top-up Service

A private service, called by the Phala Cloud billing backend, that turns finalized ERC-20
deposits into idempotent USD credits. Deposit addresses are CREATE2 forwarder contracts that
can only pay the treasury; the service runs inside a dstack confidential VM and credits products
through a signed HTTP settlement contract. The default flow is quote first: the user locks a
price, receives an exact amount and a single-use address, and pays within the window. A
persistent address is available for send-any-amount deposits.

First route: Ethereum Mainnet PHA → Phala Cloud USD credit. Further assets, chains, and products
are added through route configuration and adapters.

## Flow

```text
product registers an account; user asks for a quote (or a persistent address)
  → service locks the price and computes a CREATE2 forwarder address (no key, nothing deployed)
  → scanner reads finalized blocks and records the transfer
  → a second RPC provider confirms block hash and log; the quote is taken at that instant
  → sanctions screening and per-deposit bounds
  → signed, idempotent settlement call to the product, which verifies the log itself
  → batched flush of forwarders to the treasury
  → reconciliation of chain, service, and product ledger
```

There is no operator step and no failure state: anything that cannot complete retries with
backoff and raises an alert on age. Deterministic denials are recorded with evidence and never
credited.

## Ownership

The service owns addresses, chain evidence, finality, screening, pricing, deposit state,
settlement requests, sweeps, and reconciliation. The product owns customer identity, spendable
balance, debt, entitlements, and billing policy.

## Documents

- [Design](docs/architecture.md) — goal, trust model, schema, state machine, contracts, deployment, policies, acceptance
- [First route profile](examples/phala-cloud-pha.yaml)
- [Delivery plan](docs/plan.md) — lanes, work packages, gates, agent rules; no dates

## Status

Design v5 is under review. Implementation has not started. Production policy values are set by
finance, risk, and operations at pilot time.
