# Service configuration

This page is the reference for the `topup` binary: its commands, the flags of `topup run`, and the
environment it reads. In a deployment the attested compose sets all of these
([deploy/README.md, "Attested settings"](../deploy/README.md#attested-settings)), so an operator
changes them through the fork and a Deploy `upgrade`, not by hand. Route files are described in
[architecture §14](architecture.md#14-configuration-and-deployment).

## Commands

`topup --help` lists them; each takes `--help`.

| Command | Purpose |
|---|---|
| `topup run --route FILE…` | The service: the HTTP API and every worker loop. |
| `topup migrate` | Applies the database migrations with `MIGRATE_DATABASE_URL`. |
| `topup route validate FILE`, `topup route show FILE` | Validates a route file; prints it resolved, as JSON with every code default written out. `--template` permits the zero factory and implementation placeholders of a deployment template. |
| `topup reconcile --route FILE…` | Runs one reconciliation pass and exits. |
| `topup attest --nonce HEX --account acct_… [--live] [--version N]` | Prints the attestation evidence that `GET /v1/attestation` returns to that account. |
| `topup keys` | Derives the backup key and the database credentials from dstack into tmpfs files. |
| `topup heartbeat` | Records the RPO heartbeat every `--interval-s` seconds (60 by default). |
| `topup healthcheck` | Exits zero only when the local API answers `GET /healthz` with `200`. |
| `topup restore-check --route FILE…` | Validates a restored database and runs the post-restore reconciliation ([deploy/RESTORE.md](../deploy/RESTORE.md)). |

## Database roles

Runtime commands use `DATABASE_URL`. Its login role must be a member of the migration-created
`topup_app` NOLOGIN role. The application role has operational CRUD privileges but no
`TRUNCATE`, and append-only tables (among them `transitions`, `audit`, `events`, and the finalized
chain facts `flushed` and `flush_failures`) permit only `SELECT` and `INSERT`. The
[migrations README](../crates/topup/migrations/README.md#roles-and-privileges) lists every grant.

`topup migrate` and `topup restore-check` use only `MIGRATE_DATABASE_URL`. It must identify the
trusted database owner, with permission to create roles and schema objects; these commands never
fall back to the application URL.

## Scanner

`topup run` loads each enabled route version from a repeated `--route FILE` option. For each
chain and asset the scanner uses the highest supplied route version.

Each route names its chain's RPC providers by id in `chain.rpc_providers` (by default
`[provider-a, provider-b]`). An id resolves to the environment variable `TOPUP_RPC_<ID>_URL`,
where `<ID>` is the id upper-cased with every non-alphanumeric character replaced by `_`
(`base-sepolia-a` reads `TOPUP_RPC_BASE_SEPOLIA_A_URL`). A URL with the placeholder `{key}`
takes the API key in `TOPUP_RPC_<ID>_KEY` there, so the URL can be attested while the key stays
sealed ([deploy/README.md, "RPC providers"](../deploy/README.md#rpc-providers)). The first
provider is provider A, which scans.

| Flag | Default | Meaning |
|---|---|---|
| `--head-poll-interval-s` | one block time (12 s) | Delay between `eth_blockNumber` polls of provider A. Each new block's transfers to every issued address are read in one request. |
| `--finalized-poll-interval-s` | 60 | Least delay between reads of the `finalized` head. Its advances drive the finalized backstop, the finality watch, and reconciliation. |
| `--reconcile-interval-s` | 600 | Least delay between reconciliation rounds; a round runs only after `finalized` advanced. |
| `--wait-interval-s` | 60 | Delay before retrying an expected wait outcome. |

[Architecture §8](architecture.md#8-chain-valuation-screening) has the cadences, and
[deploy/README.md, "Measuring RPC usage"](../deploy/README.md#measuring-rpc-usage) the call
counters and a cost formula.

## HTTP API

`topup run` serves the HTTP API on `0.0.0.0:8080` by default; `--bind` overrides the socket
address.

`TOPUP_PUBLIC_ORIGIN` is required: the public scheme and authority that clients call, such as
`https://topup.example` (no path). The admin API's RFC 9421 signatures are verified against this
origin plus the request path and query, and treasury challenges (EIP-4361) name it. Behind an
ingress it must be the public URL (in a CVM, the
[custom domain](../deploy/README.md#custom-domain)), not the internal address. The service never
trusts `Host` or `X-Forwarded-*` headers.

## Environment

| Variable | Read by | Meaning |
|---|---|---|
| `DATABASE_URL` | `run`, `heartbeat`, `reconcile` | The application login role (above). |
| `MIGRATE_DATABASE_URL` | `migrate`, `restore-check` | The database owner (above). |
| `TOPUP_PUBLIC_ORIGIN` | `run` | The public origin (above). Required. |
| `TOPUP_ADMIN_KID`, `TOPUP_ADMIN_PUBLIC_KEY` | `run` | The key id and base64 ed25519 public key of the operator's admin key. Required. |
| `TOPUP_RPC_<ID>_URL`, `TOPUP_RPC_<ID>_KEY` | `run`, `reconcile`, `restore-check` | Each RPC provider's URL, and the key that fills its `{key}` placeholder (at least 8 characters of `A-Z a-z 0-9 - . _ ~`). A key without a placeholder, or a placeholder without a key, is refused. |
| `TOPUP_WEBHOOK_PROXY` | `run` | The egress proxy of webhook deliveries, such as `http://smokescreen:4750`. Required unless `TOPUP_PUBLIC_ORIGIN` is `http` (local stacks) ([webhook egress](../deploy/README.md#webhook-egress)). |
| `TOPUP_SERVICE_ENABLED` | `run`, `heartbeat` | `on` (default), `read-only` (the restore-check variant: `run` serves reads only, and Sentry reports as `<environment>-restore`), or `off`. `heartbeat` runs only while `on`. |
| `TOPUP_RESTORE_FROM_BACKUP` | `restore-check` | `on` (default) or `off`; `off` makes `restore-check` do nothing. |
| `TOPUP_RESTORE_REPORT_FILE` | `restore-check` | Where to write the restore report, if set. |
| `TOPUP_BACKUP_TIMESTAMP_FILE` | `run` | The WAL-G success marker the `topup-backup` Crons monitor reads; defaults to `/run/topup-observability/last-backup-unix-seconds`. |
| `SENTRY_DSN`, `SENTRY_ENVIRONMENT`, `TOPUP_IMAGE` | every command | Sentry reporting, off while `SENTRY_DSN` is unset or empty; the release is the `TOPUP_IMAGE` digest ([deploy/README.md, "Sentry"](../deploy/README.md#sentry)). |

The service also needs the dstack guest API socket (`/var/run/dstack.sock`) for its keys and
attestation; local stacks use the dstack simulator.

## Restore mode

A database restored from backup starts in **restore mode**
([architecture §14](architecture.md#14-configuration-and-deployment)). Reads work, every merchant
write answers `503 service_restoring`, and nothing is credited or delivered until the operator has
reconciled the restore with each merchant's records through `/v1/admin/restore/…` and unfrozen
it. `topup restore-check` validates the restored database read-only and records the restore; the
[restore guide](../deploy/RESTORE.md) and the
[reconciliation runbook](../deploy/runbooks/restore.md) have the steps.
