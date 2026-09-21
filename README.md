# Crypto Top-up Service

A private service for turning finalized crypto deposits into idempotent account credits for Phala products.

The service separates blockchain-specific deposit handling from product billing. Products register an account, request a deposit route, and receive a signed settlement event after the service has confirmed, screened, priced, and recorded a deposit.

## Core flow

```text
account requests deposit route
  → custody adapter provisions an address
  → chain adapter detects a transfer
  → finality policy confirms the canonical event
  → risk policy evaluates the deposit
  → pricing adapter locks an exchange rate
  → settlement adapter credits the destination account
  → custody adapter sweeps funds to treasury
  → reconciliation verifies every boundary
```

All recurring steps are automated. Transient failures enter persisted retry states. Deterministic denials remain uncredited and produce an auditable event.

## General model

A **route** defines:

- source chain and asset
- custody and treasury configuration
- finality policy
- risk policy
- pricing pair and quote policy
- destination product, account, and credit unit
- settlement adapter
- sweep policy

The first deployment profile is Ethereum PHA → Phala Cloud USD credit. The service model also supports additional EVM assets, chains, products, and settlement units through adapters and configuration.

## Service boundaries

The service owns:

- deposit-address lifecycle
- chain ingestion and canonical-event evidence
- finality, risk, and pricing decisions
- deposit state and idempotency
- settlement requests and receipts
- gas funding, sweeping, retries, and reconciliation

The destination product owns:

- customer/workspace identity
- spendable balance and ledger
- debt repayment and entitlement restoration
- customer-facing billing policy

Private keys remain in the configured custody or MPC/HSM system.

## Documents

- [Architecture](docs/architecture.md)
- [Phala Cloud PHA profile](examples/phala-cloud-pha.yaml)

## Initial delivery

The initial implementation targets one complete route:

```text
Ethereum Mainnet PHA → Phala Cloud USD credit
```

The route exercises the generic adapter contracts and automated lifecycle before additional assets or products are enabled.

## Status

Architecture and implementation planning are under review. Production policy thresholds remain runtime configuration owned by risk, finance, and operations.
