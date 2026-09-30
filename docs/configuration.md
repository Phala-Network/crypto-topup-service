# Service configuration

This page is the reference for the `topup` binary: its commands, its configuration file, its flags,
and the few environment variables it reads. A deployment's configuration file is committed in its
environment directory in the operator's environment repository, for example
`production/topup/topup.yaml` (Phala's staging:
`deploy/environments/phala-network/staging/topup/topup.yaml`), and inlined into the attested
compose. A change is therefore a pull request and a Deploy `upgrade`
([deploy/README.md, "Attested settings"](../deploy/README.md#attested-settings)). The only
environment variables are the database login's and the two kinds of secret the owner seals into
the CVM's encrypted environment: `SENTRY_DSN` and each `TOPUP_RPC_<ID>_KEY`
([deploy/README.md, "Sealing the secrets"](../deploy/README.md#sealing-the-secrets)). The design
is in [design/deploy-config.md](design/deploy-config.md).

## Commands

`topup --help` lists them; each takes `--help`.

| Command | Purpose |
|---|---|
| `topup run --config FILE` | The service: the HTTP API and every worker loop. |
| `topup migrate` | Applies the database migrations as the database owner. |
| `topup config check [--secrets] FILE`, `topup config show FILE` | Validates a configuration file without any secret; prints it resolved, as JSON with every route default written out and each keyed provider's URL still carrying its `{key}`. `--secrets` also checks each provider's sealed key (`TOPUP_RPC_<ID>_KEY`) against its URL and prints no value. |
| `topup route validate FILE`, `topup route show FILE` | The same for one route file (`examples/`, the sandbox template). `--template` permits the zero factory and implementation placeholders of a deployment template. |
| `topup reconcile --config FILE` | Runs one reconciliation pass and exits. |
| `topup attest --nonce HEX --account acct_… [--live] [--version N]` | Prints the attestation evidence that `GET /v1/attestation` returns to that account. |
| `topup keys` | Derives the backup key and the database credentials from dstack into tmpfs files. |
| `topup heartbeat` | Records the RPO heartbeat every `--interval-s` seconds (60 by default). |
| `topup healthcheck` | Exits zero only when the local API answers `GET /healthz` with `200`. |
| `topup restore-check --config FILE [--report FILE]` | Validates a restored database and runs the post-restore reconciliation ([deploy/RESTORE.md](../deploy/RESTORE.md)). |

## The configuration file

One YAML file, parsed with unknown fields refused, holds every public setting of a deployment:

```yaml
environment: staging                       # the Sentry environment: a tag, never a policy input
public_origin: https://pay-api-staging.phala.com
admin_key:
  id: admin/staging-v1                     # the key id admin requests sign with
  public_key: 23Y9wEJMOTySGV3UXmcTFnQsbigA9/cYTvmqdQxzmdo=   # standard base64 ed25519
rpc_providers:                             # id → URL
  provider-a: https://sepolia.gateway.tenderly.co
  provider-b: https://eth-sepolia.g.alchemy.com/v2/{key}
routes:                                    # every enabled route version, as route files are written
  - route: phala-cloud-sepolia-pha-usd
    version: 3
    ...
```

- **`public_origin`** is the public scheme and authority clients call (no path). The admin API's
  RFC 9421 signatures are verified against it plus the request path and query, and treasury
  challenges (EIP-4361) name it. Behind an ingress it must be the public URL (in a CVM, the
  [custom domain](../deploy/README.md#custom-domain)), not the internal address. The service never
  trusts `Host` or `X-Forwarded-*` headers.
- **`public_origin`** and **`admin_key.public_key`** may be left out only for `topup run` to take
  them from its environment (`--public-origin-host-env`, `--admin-public-key-env`): the Phala Cloud
  template's, whose deploy form holds them
  ([deploy/README.md](../deploy/README.md#the-phala-cloud-template-variant)). `topup run` refuses
  to start unless each has exactly one source. Every other deployment writes both, attested.
- **`rpc_providers`** maps each provider id (lowercase letters, digits, `-`) to its URL, `https` in
  a deployment (preflight requires it). Routes name their chain's providers by id in
  `chain.rpc_providers`, and a route that names none uses `provider-a` and `provider-b`. The first
  provider is provider A, which scans. A provider that puts an API key in its URL is configured
  with the placeholder `{key}` where its documentation puts the key. `{key}` must be a whole path
  segment or a whole query value, and filling it must leave the scheme, host, and port unchanged.
  The key itself is the sealed `TOPUP_RPC_<ID>_KEY` (the id upper-cased, every non-alphanumeric
  character `_`): at least 8 characters of `A-Z a-z 0-9 - . _ ~`. A key without a placeholder, or
  a placeholder without a key, is refused.
- **`routes`** are route files, one list item each; their fields and defaults are in
  [architecture §14](architecture.md#14-configuration-and-deployment).

`topup config check` refuses a file that the service would refuse, without reading a secret.
Preflight runs it in the compose's pinned image; to run it by hand, use the release's image:

```sh
docker run --rm -i "$(jq -r '."phala-pay"' images.json)" topup config check /dev/stdin \
  <production/topup/topup.yaml
```

The files it refuses include:

- an invalid origin or admin key;
- a route that fails validation, or routes that disagree on a chain;
- a provider a route names but the file does not configure, or one no route names;
- a provider named on two chains;
- a chain whose providers share a URL;
- a misplaced `{key}`.

## Flags

`topup run`:

| Flag | Default | Meaning |
|---|---|---|
| `--config FILE` | required | The configuration file. |
| `--bind` | `0.0.0.0:8080` | The API's socket address. |
| `--webhook-proxy URL` | none | The egress proxy every webhook delivery goes through, the smokescreen sidecar in a CVM ([webhook egress](../deploy/README.md#webhook-egress)). Required unless the public origin is `http` (local stacks). |
| `--read-only` | off | Serves only the read API of a database restored from backup (the restore-check variant, [deploy/RESTORE.md](../deploy/RESTORE.md)): no loop, no lease-owner lock, every write refused, and Sentry reports as `<environment>-restore`. |
| `--restore-report FILE` | none | With `--read-only`, the restore-check report `/healthz` serves. |
| `--public-origin URL` | the file's | Replaces `public_origin`: the restore instance's own origin, or a local stack's. The attested service compose never sets it. |
| `--public-origin-host-env NAME` | none | When the file leaves `public_origin` out: the environment variable holding the origin's host, a lowercase DNS name served as `https://HOST` (the template's `DSTACK_APP_DOMAIN`). |
| `--admin-public-key-env NAME` | none | When the file leaves `admin_key.public_key` out: the environment variable holding the admin public key, standard base64 ed25519 (the template's `TOPUP_ADMIN_PUBLIC_KEY`). |
| `--head-poll-interval-s` | one block time (12 s) | Delay between `eth_blockNumber` polls of provider A. Each new block's transfers to every issued address are read in one request. |
| `--finalized-poll-interval-s` | 60 | Least delay between reads of the `finalized` head. Its advances drive the finalized backstop, the finality watch, and reconciliation. |
| `--reconcile-interval-s` | 600 | Least delay between reconciliation rounds; a round runs only after `finalized` advanced. |
| `--wait-interval-s` | 60 | Delay before retrying an expected wait outcome. |

[Architecture §8](architecture.md#8-chain-valuation-screening) has the cadences, and
[deploy/README.md, "Measuring RPC usage"](../deploy/README.md#measuring-rpc-usage) the call
counters and a cost formula.

## Database roles

Every database command reads `DATABASE_URL`, with its password from libpq's `PGPASSFILE`.

`run`, `heartbeat`, and `reconcile` log in as the application role. That login must be a member of
the migration-created `topup_app` NOLOGIN role. The application role has operational CRUD
privileges but no `TRUNCATE`. Append-only tables (among them `transitions`, `audit`, `events`, and
the finalized chain facts `flushed` and `flush_failures`) permit only `SELECT` and `INSERT`. The
[migrations README](../crates/topup/migrations/README.md#roles-and-privileges) lists every grant.

`migrate` and `restore-check` must log in as the trusted database owner, with permission to create
roles and schema objects. They check it when they connect and refuse any other login, the
application role included, before touching the schema.

## Environment

| Variable | Read by | Meaning |
|---|---|---|
| `DATABASE_URL` | database commands | The login above; its password is in `PGPASSFILE`. |
| `TOPUP_RPC_<ID>_KEY` | `run`, `reconcile`, `restore-check`, `config check --secrets` | The sealed key that fills provider `<ID>`'s `{key}`. |
| `DSTACK_APP_DOMAIN`, `TOPUP_ADMIN_PUBLIC_KEY` | `run`, only when named by the flags above | The Phala Cloud template's origin host and admin key. |
| `SENTRY_DSN` | `run` | Sentry reporting, off while unset or empty ([deploy/README.md, "Sentry"](../deploy/README.md#sentry)). The environment is the file's `environment`; the release is the source commit compiled into the image. |

The service also needs the dstack guest API socket (`/var/run/dstack.sock`) for its keys and
attestation; local stacks use the dstack simulator (`DSTACK_SIMULATOR_ENDPOINT`).

## Restore mode

A database restored from backup starts in **restore mode**
([architecture §14](architecture.md#14-configuration-and-deployment)). Reads work, every merchant
write answers `503 service_restoring`, and nothing is credited or delivered until the operator has
reconciled the restore with each merchant's records through `/v1/admin/restore/…` and unfrozen
it. `topup restore-check` validates the restored database read-only and records the restore; the
[restore guide](../deploy/RESTORE.md) and the
[reconciliation runbook](../deploy/runbooks/restore.md) have the steps.
