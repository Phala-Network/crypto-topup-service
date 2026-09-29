# Phala Pay documentation

Documentation is grouped by who reads it. New to Phala Pay? Start with
[How Phala Pay works](overview.md).

## Integrate (merchants)

For a merchant's backend team, connecting an account to an operator's instance.

| Document | What it covers |
|---|---|
| [Integration guide](integration.md) | The quickstart, quotes, deposit addresses, treasuries, sweeps, webhooks and fulfillment, refunds, testing, go-live, and the API reference by topic. |
| [API reference](https://phala-network.github.io/phala-pay/) | Every endpoint and schema, built from [crates/topup/openapi.json](../crates/topup/openapi.json) ([how it is built](reference/README.md)). |
| [`@phala/pay`](../sdk/js/README.md) | The browser checkout and the Node server helpers. |
| [`phala-pay`](../sdk/python/README.md) | The Python client. |
| [Integrator sandbox](../deploy/sandbox/README.md) | Scripted payment scenarios, locally or against an operator's test mode. |

## Self-host and operate (operators)

For the operator who runs an instance, onboards merchants, and handles incidents.

| Document | What it covers |
|---|---|
| [Self-hosting guide](self-hosting.md) | The steps, in order, from a fork to a credited test deposit, and on to going live. |
| [Deployment reference](../deploy/README.md) | GitHub setup, releases and deploys, sealed secrets, attested settings, RPC providers, the custom domain, Sentry, attestation, onboarding, and Phala's own instance. |
| [Service configuration](configuration.md) | The `topup` commands, flags, and environment. |
| [Contract deployment](../deploy/CONTRACTS.md) | Deploying and verifying the deterministic forwarder factory on a chain. |
| [Backup and restore](../deploy/RESTORE.md) | WAL-G backups, the restore-check variant, restores, and drills. |
| [Runbooks](../deploy/runbooks/README.md) | One runbook per alert or incident, and the alert index. |

## Reference

| Document | What it covers |
|---|---|
| [Architecture](architecture.md) | The specification: trust model, contracts, schema, state machine, chain reads, quotes, webhooks, API, reconciliation, deployment, and policies. |
| [Forwarder contracts](../contracts/README.md) | The `ForwarderFactory` and forwarder model, flush semantics, and tests. |
| [Database migrations](../crates/topup/migrations/README.md) | Database roles and grants, and what each migration changes. |
| [Changelog](../CHANGELOG.md) | Integrator-visible changes to the HTTP API and webhooks; the SDKs keep their own ([JS](../sdk/js/CHANGELOG.md), [Python](../sdk/python/CHANGELOG.md)). |

## Design and internals

| Document | What it covers |
|---|---|
| [How Phala Pay works](overview.md) | The model, the payment lifecycle, and who owns what. |
| [Design: multi-tenant Phala Pay](design/multi-tenant.md) | The decision record behind the current design, with its amendments. |
| [Plan to production](plan.md) | What remains before Phala's own instance goes live. |

## Contributing

[CONTRIBUTING.md](../CONTRIBUTING.md) covers the development setup, tests, conventions, and
releases. Report vulnerabilities as described in [SECURITY.md](../SECURITY.md).
