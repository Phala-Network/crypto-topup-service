# Phala Pay

A service, called by the Phala Cloud billing backend, that turns finalized ERC-20
deposits into idempotent USD credits. Deposit addresses are CREATE2 forwarder contracts that
can only pay the treasury; the service runs inside a dstack confidential VM and tells products what
to credit with signed `deposit.credited` webhooks, which they fulfill once per deposit. The default flow is quote first: the user locks a
price, receives an exact amount and a single-use address, and pays within the window. Each
customer can also have one persistent, rotatable deposit address for every supported token on
every chain (the same address wherever the treasury is the same), credited at spot for any amount, like the stable bank-transfer details of Stripe's customer balance.

First route: Ethereum Mainnet PHA → Phala Cloud USD credit. Further assets, chains, and products
are added through route configuration and adapters.

## Flow

```mermaid
flowchart LR
    payer(["Payer"])
    subgraph product["Product (e.g. Phala Cloud)"]
        ui["Web app<br/>&lt;Checkout&gt; from @phala/pay"]
        backend["Backend<br/>PhalaPay SDK"]
    end
    subgraph cvm["Phala Pay (dstack CVM, attested)"]
        api["HTTP API<br/>/v1/quotes, deposits, refunds"]
        worker["Scanner, pump,<br/>outbox, reconciler"]
    end
    subgraph chain["Ethereum"]
        fwd["CREATE2 forwarders<br/>(one per quote)"]
        safe[("Treasury Safe")]
    end
    payer -->|"wallet, QR, or manual transfer"| fwd
    ui -->|"client_secret: status"| api
    ui <--> backend
    backend -->|"signed: create quote, refunds"| api
    worker -->|"signed deposit.credited webhook"| backend
    worker -->|"reads finalized logs (2 RPC providers)"| fwd
    sweeper(["Merchant wallet or Safe<br/>(anyone may flush)"]) -->|"factory flush, pays gas"| fwd
    fwd -->|"can only pay"| safe
```

```text
product asks for a quote; the account is created with it
  → service locks the price and computes a CREATE2 forwarder address (no key, nothing deployed)
  → the per-block scan shows the payment as "seen, N confirmations" within seconds of its block
  → and records the transfer once its block reaches the route's confirmation (2 on Ethereum)
  → a second RPC provider confirms block hash and log; the quote is taken at that instant
  → sanctions screening and per-deposit bounds
  → credited: a signed deposit.credited webhook, retried until the product fulfills it once
  → the merchant (or anyone) flushes forwarders to the treasury; the service sends no transaction
    and marks deposits swept from the finalized Flushed events
  → reconciliation of chain, service, and product ledger
```

There is no operator step and no failure state: anything that cannot complete retries with
backoff and raises an alert on age. Deterministic denials are recorded with evidence and never
credited.

## Ownership

The service owns addresses, chain evidence, finality, screening, pricing, deposit state,
credits and their webhooks, the swept status it reads from the chain, and reconciliation. The
merchant sweeps its forwarders and pays that gas. The product owns customer identity, spendable
balance, debt, entitlements, and billing policy.

## Documents

- [Design](docs/architecture.md) — goal, trust model, schema, state machine, contracts, deployment, policies, acceptance
- [First route profile](examples/phala-cloud-pha.yaml)
- [Integration guide](docs/integration.md) — quickstart, quotes, webhooks and fulfillment, refunds, testing, reference
- [Plan to production](docs/plan.md) — what is done, what remains, and who owns it

## Database roles

Runtime commands use `DATABASE_URL`, whose login role must be a member of the migration-created
`topup_app` NOLOGIN role. The application role has operational CRUD privileges but no `TRUNCATE`,
and append-only `transitions` and `audit` permit only `SELECT` and `INSERT`.

`topup migrate` uses only `MIGRATE_DATABASE_URL`. It must identify the trusted database owner with
permission to create roles and schema objects; the command never falls back to the application URL.

## Scanner configuration

`topup run` reads `DATABASE_URL` and accepts each enabled route version through a repeated
`--route FILE` option. Provider ids in each route file resolve to `TOPUP_RPC_<ID>_URL` after
uppercasing and replacing non-alphanumeric characters with underscores; a URL with the placeholder
`{key}` takes the API key in `TOPUP_RPC_<ID>_KEY` there, so the URL can be attested while the key
stays sealed (deploy/README.md, "Sealing the secrets"); the first provider is
provider A for scanning. For each chain and asset the scanner uses the highest supplied route
version. A head loop on provider A polls `eth_blockNumber` once per block time
(`--head-poll-interval-s`, 12 seconds by default) and reads each new block's transfers to every
issued address in one request; `finalized` is read at most every `--finalized-poll-interval-s`
(60 seconds), and its advances drive the finalized backstop, the finality watch, and reconciliation
(`--reconcile-interval-s`, at most every 600 seconds). docs/architecture.md §8 has the cadences,
and deploy/README.md ("Measuring RPC usage") the call counters and a cost formula.

The same command serves the HTTP API on `0.0.0.0:8080` by default; `--bind` overrides the socket
address. `TOPUP_PUBLIC_ORIGIN` is required: the public scheme and authority clients call, such as
`https://topup.example` (no path). Request signatures are verified against this origin plus the
request path and query, so behind an ingress it must be the public URL (in a CVM, the custom
domain of `deploy/README.md`), not the internal address; `Host` and `X-Forwarded-*` headers are
never trusted.

## Restore

`topup restore-check` runs the architecture §13 post-restore reconciliation, which GETs and
adopts the product's answer for every deposit at or beyond `cleared`. Run it only while `topup`, `heartbeat`, and `backup` are stopped; the
[restore runbook](deploy/RESTORE.md) keeps the service stopped until the check reports `ok`.

## Status

Staging runs on Sepolia at `https://pay-api-staging.phala.com`. Nothing is deployed to mainnet:
[the plan to production](docs/plan.md) lists the remaining integration, inputs, and reviews.

## License

[Apache-2.0](LICENSE). Report vulnerabilities as described in [SECURITY.md](SECURITY.md).
