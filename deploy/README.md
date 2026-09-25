# dstack staging deployment

This directory implements work packages D1 and D2 from `docs/plan.md`: images, deployment
artifacts, and the CI workflows that deploy staging. D3 encrypted backups, restore, and restore
drills are documented in [RESTORE.md](RESTORE.md).

Staging is deployed only by GitHub Actions; nothing is deployed from a laptop. Commands outside
those workflows that change a registry, Phala Cloud, a CVM, a Safe, an on-chain contract, or secret
state are marked **HUMAN-ONLY** and describe the production path (on-chain Base KMS, Finance Safe).
The commands were checked on 2026-09-22 against dstack commit
`721df1b93fd93884224f2261c37dd86ca250432f` and Phala Cloud CLI `phala` 1.1.22 (tag `cli-v1.1.22`
of `Phala-Network/phala-cloud`); the SDK and the simulator now target the dstack `v0.5.9` release,
commit `282eeb27d22d8f091ad0fa5a90e638f85cf68751` ([OS image](#os-image)), and the linked dstack
documents are those of that commit.

## First staging deploy checklist

Release images publishes the images; the other workflows run against the GitHub Environment
`staging`:

| Workflow | Trigger | Does |
|---|---|---|
| Release images ([release-images.yml](../.github/workflows/release-images.yml)) | manual, `main` only | builds and publishes `ghcr.io/phala-network/crypto-topup`, `postgres-walg`, and `crypto-topup-reference-product` ([Build and publish images](#build-and-publish-images)); platform manifest references in the job summary and the `images.json` artifact |
| Deploy staging ([deploy-staging.yml](../.github/workflows/deploy-staging.yml)) | manual, `main` only | provisions or upgrades the staging CVM (below) |
| Deploy staging product ([deploy-staging-product.yml](../.github/workflows/deploy-staging-product.yml)) | manual, `main` only | provisions or upgrades the staging reference-product CVM ([Staging reference product](#staging-reference-product)) |
| Verify contracts ([verify-contracts.yml](../.github/workflows/verify-contracts.yml)) | daily and manual | read-only: `verify-safe.sh`, `verify-deployment.sh` on both Sepolia providers, `topup route validate` on the committed route; JSON reports as artifacts |

### Controls

The `staging` Environment admits only the `main` branch, and the deploy jobs also check
`github.ref`. Starting a workflow needs write access to the repository. Required reviewers are not
available for a private repository on the current GitHub plan, so there is **no approval gate**:
dispatching Deploy staging is the decision, and the run's actor is the record.
GitHub holds exactly one secret, the `staging` Environment secret `PHALA_CLOUD_API_KEY`, which
reaches the CLI through an environment variable, never a command-line argument. No runtime secret
is stored in GitHub: the S3 keys and the Coin Metrics key are sealed into the CVM by the owner from
their own machine (see [Deploy](#deploy)). No signing key is stored in GitHub:
contract deployments and Safe transactions are signed by the Safe owner outside CI. The workflows have
`permissions: contents: read`, pin every action by commit SHA, and serialize runs
(`deploy-staging`) without cancelling one in progress.

Mainnet is never deployed from CI. Mainnet contracts and any production CVM stay HUMAN-ONLY
through the Finance Safe ([CONTRACTS.md](CONTRACTS.md#mainnet), sections A and B below).

### OS image

The owner-approved OS image is `dstack-0.5.9`: dstack 0.5.9, the latest general-availability
release, non-dev. It replaces `dstack-0.6.0-rc5`, which `os-images --prod` lists but no Phala
Cloud node offers (checked on 2026-09-24 in both workspaces: every node offers only `dstack-0.5.8`,
`dstack-0.5.9`, and their dev and nvidia variants), so provisioning with it failed with "OS image …
is not available on the selected node" (ERR-02-013). The local simulator is built from the same
release (commit `282eeb27d22d8f091ad0fa5a90e638f85cf68751`, tag `v0.5.9`), and the service uses
`dstack-sdk = "=0.1.3"` from crates.io, the `rust-sdk-v0.5.9` source with only `hickory-dns`
dropped from its `reqwest` features. Both speak the dstack 0.5 guest API on
`/var/run/dstack.sock` (`/GetKey`, `/Attest`, `/Info`). [preflight.sh](preflight.sh) accepts only this image name and,
online, requires `os-images --prod` to list it as a non-dev 0.5.9 image and at least one node of
the workspace (`api /teepods/available`) to offer it, with on-chain KMS support for `--kms base`.

The dstack 0.5 key derivation differs from 0.6's `/v1` API: a key depends on the app identity and
the domain only, not on the algorithm, and 0.6 derives different keys for the same domain. Keys
derived on 0.5.9 (operator address, settlement key, backup and database keys) therefore change if
the app moves to a dstack 0.6 image. Such a move is a key migration with its own runbook
(operator role grant, settlement-key re-pinning, backup re-encryption, database password change),
not an OS image bump.

### One-time setup (HUMAN-ONLY, repository owner)

1. **Phala Cloud API key.** Create an API key in the Phala Cloud dashboard for the workspace
   "kingsley's projects" and store it as the Environment secret `PHALA_CLOUD_API_KEY` (done). The
   CLI reads it from that variable; the workflow gives the CLI an empty configuration directory, so
   no stored login profile is ever used. The workflow checks the workspace display name.
2. **Environment secret and variables** of `staging`. The CVM's env holds only the owner-sealed
   secrets, exactly the names of [staging.env.example](staging.env.example); they never enter
   GitHub. Everything else is a public setting that Deploy staging renders into the attested
   compose ([Attested settings](#attested-settings)).

   | Name | Kind | Value |
   |---|---|---|
   | `PHALA_CLOUD_API_KEY` | secret (the only one) | Phala Cloud API key (step 1) |
   | `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | owner-sealed, not in GitHub | S3/R2 key for the WAL-G bucket |
   | `SENTRY_DSN` | owner-sealed, optional | DSN of the Sentry project `phala-network/crypto-topup-service`; empty turns reporting off ([Sentry](#sentry)) |
   | `AWS_ENDPOINT` | variable, attested | S3-compatible HTTPS endpoint (R2: `https://<account>.r2.cloudflarestorage.com`; AWS: `https://s3.<region>.amazonaws.com`) |
   | `AWS_REGION` | variable, attested | bucket region (`auto` for R2) |
   | `AWS_S3_FORCE_PATH_STYLE` | variable, attested | `false` (or `true` for a path-style endpoint) |
   | `WALG_S3_PREFIX` | variable, attested | `s3://BUCKET/PATH`; a new app needs a prefix of its own ([RESTORE.md](RESTORE.md#bootstrap-from-backup)) |
   | `TOPUP_ADMIN_KID`, `TOPUP_ADMIN_PUBLIC_KEY` | variable, attested | from `topup-sdk keygen`; the private key stays with the admin |
   | `TOPUP_BACKUP_KEY_VERSION`, `TOPUP_BACKUP_KEY_FALLBACK_VERSIONS` | variable, attested | `1` and `0` |
   | `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | variable, attested | keyless public Sepolia HTTPS RPC URLs from two different providers (also used by Verify contracts and Deploy staging product); they are published with the compose, so never a URL with an embedded key. The chain must carry the canonical Multicall3 (`0xcA11bde05977b3631167028862bE2a173976CA11`, [`contracts/multicall3.json`](contracts/multicall3.json)): balance and `addressOf` reads are aggregated through it, and `topup run` refuses to start without it |
   | `DSTACK_OS_IMAGE` | variable | the owner-approved OS image, `dstack-0.5.9` ([OS image](#os-image)) |
   | `STAGING_CVM_ID` | variable | empty until the first provisioning; then the CVM id it reports |

   `TOPUP_PUBLIC_ORIGIN` is not stored: Deploy staging derives it (below). There is no database
   secret ([Database credentials](#database-credentials)).
3. **Package visibility.** After the first Release images run, make both packages
   (`crypto-topup` and `postgres-walg` under the `phala-network` organization) public, as in
   [Build and publish images](#build-and-publish-images). The organization must first allow public
   container packages, and making a package public is irreversible. The CVM pulls them without
   credentials, and the preflight fails on an image it cannot pull anonymously.
4. **Owner decisions.** Staging uses Phala Cloud's KMS (`--kms phala`): no `DstackApp` contract, no
   provisioner key, and upgrades apply without an on-chain compose-hash approval. The on-chain
   Base KMS stays the production path (sections A and B). The staging `settlement_url` in
   `deploy/config/routes/phala-cloud-sepolia-pha.yaml` is the reference-product CVM's
   `/settlements` endpoint ([Staging reference product](#staging-reference-product)).

### Attested settings

A value in the CVM's encrypted env is outside the attestation: whoever can run `phala envs
update` could change it without changing the compose hash. So the env holds only secrets, and
[render-compose.sh](render-compose.sh) writes every other value into the compose, where the
compose hash, and so the attestation, covers it. In
[docker-compose.yml](docker-compose.yml), a `${NAME:-}` whose `NAME` is in
[staging.env.example](staging.env.example) stays a reference to the encrypted env; every other
`${NAME:-}` is a setting, taken from the environment (Deploy staging: the `staging` Environment
variables above) and refused unless it is 1-512 printable ASCII characters without spaces,
quotes, backslashes, or `$`. The renderer also writes:

- the mode switches and `topup`'s published port, from the variant it renders, never from the
  environment: the service (`TOPUP_RESTORE_FROM_BACKUP=off`, `TOPUP_SERVICE_ENABLED=on`,
  `TOPUP_INGRESS_PORT=8080`), or with `--restore-check` the restore verification instance of
  [RESTORE.md](RESTORE.md#the-restore-check-variant) (`on`, `read-only`, `8081`); the two variants
  have different compose hashes;
- the label `crypto-topup.rendered-sha256` on every service, the SHA-256 of the rendered file.
  Compose recreates a container only when its service definition changes, not when an inline
  config does, so the label makes every rendered change recreate every service.

To change a setting, change the variable and run Deploy staging `upgrade`; `envs update` changes
only secrets. [product/render-compose.sh](product/render-compose.sh) renders the reference
product's compose the same way, and the local stacks ([local/compose.sh](local/compose.sh), the
drill, the rehearsal) with local values.

The Sepolia contracts (factory, test PHA token, sanctions oracle) are deployed and committed in the
route. A future factory deployment is run by the Safe owner with their own key, as
[CONTRACTS.md](CONTRACTS.md#sepolia) describes; its constructor inputs come from the committed
`deploy/contracts/safe-expectations.json`, and its addresses reach the route through a
reviewed route PR (`deploy/config/routes/phala-cloud-sepolia-pha.yaml` and the inline copy in
`deploy/docker-compose.yml`, checked by `deploy/validate-compose.sh`).

### Deploy

1. Merge the change to `main`.
2. Run **Release images** on `main` (the only ref it publishes from); copy both
   `repository@sha256:<digest>` references from its summary or its `images.json` artifact.
3. Run **Deploy staging** on `main` with `mode: provision`, `topup_image`, and
   `postgres_walg_image`. The run refuses `provision` while `STAGING_CVM_ID` is set.
4. Set the `staging` variable `STAGING_CVM_ID` to the CVM id in the run summary. From then on use
   `mode: upgrade` with new digests; an upgrade sends only the compose, never an env file, so the
   sealed env stays.
5. **HUMAN-ONLY, owner: seal the runtime secrets.** The new CVM waits for its storage
   credentials: PostgreSQL bootstraps an empty data directory only after listing the backup prefix
   ([RESTORE.md](RESTORE.md#bootstrap-from-backup)), so without them it refuses to initialize and
   the service is not up. From your own machine, write `.env.staging` (mode 0600) with exactly
   the names of [staging.env.example](staging.env.example): `AWS_ACCESS_KEY_ID`,
   `AWS_SECRET_ACCESS_KEY`, and `SENTRY_DSN` (may be empty), and seal it; the same name set keeps
   the compose hash. From a checkout of the deployed commit, with the rendered compose from the
   run's artifact:

   ```sh
   deploy/preflight.sh --env .env.staging --compose docker-compose.staging.yml --offline
   npx --yes phala@1.1.22 envs update "$STAGING_CVM_ID" -e .env.staging
   ```

   Without `--unsealed`, preflight requires the S3 keys. The CVM restarts, PostgreSQL finds the
   prefix empty and initializes a new cluster, and `/healthz` answers. Backups have started when
   the WAL key-version marker, rewritten by every archived segment, is younger than two minutes
   (the `heartbeat` service forces one segment a minute):

   ```sh
   aws s3 ls "${WALG_S3_PREFIX%/}/key-versions/current.json" --endpoint-url "$AWS_ENDPOINT"
   ```

   Re-seal the same way whenever a secret must change; a setting changes only through `upgrade`.

   **Recovery from a failed provision.** If the run fails after `phala deploy` created the CVM
   (while waiting, setting the origin, or verifying the attestation), the summary already shows
   the CVM id. Set `STAGING_CVM_ID` to it and re-run with `mode: upgrade` and the same digests,
   which renders the real origin and repeats the checks (it waits for `/healthz`, so seal the
   secrets as in step 5 first). Do not re-run `provision`, which would create a second CVM. If
   the run failed before the CVM was created (no CVM id in the summary), fix the cause and re-run
   `provision`.
6. **HUMAN-ONLY, verifier:** complete [Attestation, ingress, and egress](#attestation-ingress-and-egress)
   (the dstack verifier on the nonce-bound settlement key, the egress restriction) before
   issuing product credentials ([Product credentials](#product-credentials)).

Deploy staging, in order; any failure stops the run:

1. checks the inputs (both images `repository@sha256:<64 hex>`), the mode against
   `STAGING_CVM_ID`, and that `PHALA_CLOUD_API_KEY`, `DSTACK_OS_IMAGE`, and every attested setting
   are set;
2. resolves `TOPUP_PUBLIC_ORIGIN`: `https://pending.invalid` for a new CVM, and for an upgrade
   `https://<app_id>-8080.<gateway.base_domain>` of the existing CVM (`cvms get --json`);
3. writes the unsealed env file (`mktemp`, mode 0600, under the runner's temporary directory) with
   [write-staging-env.sh](write-staging-env.sh): the owner-sealed names, all empty;
4. renders the compose of the checked-out commit with the input digests and the settings
   (`render-compose.sh`, [Attested settings](#attested-settings));
5. runs [preflight.sh](preflight.sh) `--kms phala --unsealed`: env file, compose (identical to a
   fresh render with its images and settings, the service variant, variables equal to the env
   names, settings well formed, route without placeholder addresses), anonymous pulls of both
   images, `topup route validate`, both RPC providers' chain id, `verify-deployment.sh` against
   both providers, the route's token and oracle code, the Phala workspace, and that
   `DSTACK_OS_IMAGE` is the approved `dstack-0.5.9`, a listed production image offered by a node;
6. `phala deploy` (CLI 1.1.22 via `npx`): a new CVM with `--kms phala --instance-type tdx.medium
   --fs ext4 --image "$DSTACK_OS_IMAGE" --no-dev-os --no-public-logs --no-public-sysinfo
   --public-tcbinfo --secure-time` and the unsealed env file, or an update with
   `--cvm-id "$STAGING_CVM_ID" --wait` and no `-e`: CLI 1.1.22 then sends neither `allowed_envs` nor
   `encrypted_env` (`resolveEnvVars` and `updateCvm` in `cli/src/commands/deploy/handler.ts` at tag
   `cli-v1.1.22`), so the sealed env and its name set stay;
7. waits until the CVM is `running` with no operation in progress and records the CVM's compose
   hash; an upgrade also waits until `/healthz` answers at the gateway URL (a new CVM waits for
   its secrets, step 5 above);
8. for a new CVM, renders the compose again with the gateway URL as `TOPUP_PUBLIC_ORIGIN`, checks
   it offline, and upgrades the CVM to it (`deploy --cvm-id`, no `-e`), then waits (at most 15
   minutes) for `running` with the new compose hash. An upgrade skips this step;
9. polls `cvms attestation` (at most 10 minutes) until its event log records the deployed compose
   hash, so an upgrade never checks the previous compose, reads the guest agent's public info
   (port 8090) for the CVM's vm_config, and runs [verify-attestation.sh](verify-attestation.sh)
   against the rendered compose (see [Deployment artifacts](#deployment-artifacts));
10. records the CVM id, app id, compose hash, images, and origin in the job summary (for a new
    CVM also the owner's sealing commands of step 5 above); uploads the rendered compose,
    `deploy.json`, `cvm.json`, `attestation.json`, `info.json`, and the verification output
    (no secrets); deletes the env file even when a step failed.

`make cvm-rehearsal` runs the same artifact locally against Anvil, Garage (S3), and the dstack simulator
(see [Local verification](#local-verification)).

## Deployment artifacts

- `docker-compose.yml` is the measured workload. Render immutable image references into a separate
  staging file before giving it to the CLI.
- `staging.env.example` lists every encrypted environment variable, the owner-sealed secrets; none
  is a database credential. Every other value is an attested setting
  ([Attested settings](#attested-settings)).
- `app-compose.example.json` and `render-app-compose.sh` are review previews of the fields CLI
  1.1.22 constructs. They are not authoritative deployment manifests or authorization artifacts.
- `dstack-verifier.sh` runs the official dstack verifier of dstack v0.5.9
  (`dstacktee/dstack-verifier:0.5.9`, pinned by digest; built from the same dstack commit as the
  `dstack-0.5.9` guest agent) on a request on stdin: it verifies the TDX quote and its TCB with
  Intel's collateral, replays the event log against RTMR3, and recomputes the OS image
  measurements from the vm_config's `os_image_hash`. It needs Docker and network access to Intel's
  PCS and `download.dstack.org`; nothing is bind-mounted.
- `verify-attestation.sh ATTESTATION_JSON INFO_JSON APP_ID COMPOSE` feeds it a CVM's quote and
  event log (`phala cvms attestation --json`) and vm_config (the guest agent's public
  `GET /prpc/Info` on port 8090), then requires TCB `UpToDate`, the replayed app id `APP_ID`, and
  the replayed compose hash equal to the SHA-256 of the attested app-compose, whose
  `docker_compose_file` must be `COMPOSE` byte for byte. Finally it checks the compose policy:
  `allowed_envs` equal to the reviewed secret names and to the only variables the compose reads,
  no `MIGRATE_DATABASE_URL` in `topup`, and the single `topup` ingress: 8080, or 8081 for the
  restore-check variant ([RESTORE.md](RESTORE.md#addressing-the-restore-check-instance)) (for the
  reference product, `ENV_EXAMPLE SERVICE:PORT`).
- `Dockerfile.postgres-walg` supplies PostgreSQL 18 plus WAL-G and the D3 wrappers for encrypted,
  key-versioned WAL archiving and restore; see [RESTORE.md](RESTORE.md) for the procedure and drills.
- `alerts/prometheus-rules.yml` and `dashboards/crypto-topup-service.json` are the Prometheus and
  Grafana artifacts for §16. `GET /metrics` is intentionally unauthenticated and is served on the
  separate `--metrics-bind` listener (default `127.0.0.1:9464`). The measured compose binds that
  listener to the container network on port 9464 with `expose`; it is not published through the
  port-8080 gateway. Only the monitoring collector may reach it; no collector runs in a CVM, which
  reports to Sentry instead ([Sentry](#sentry)). The local compose publishes it on
  loopback port 19464 for smoke testing. `make alerts-check` (`check-alerts.sh`) runs
  `promtool check rules` and the alert unit tests in `alerts/prometheus-rules.test.yml` with the
  pinned Prometheus image.

## Sentry

Production CVMs have no logs and nothing scrapes `/metrics`, so the service reports to the Sentry
project `phala-network/crypto-topup-service` itself, using the official
[`sentry`](https://docs.rs/sentry/0.49.3) crate (`crates/topup/src/observability/reporting.rs`).
It is on only while the owner-sealed `SENTRY_DSN` is non-empty: without it no client is created,
no tracing layer is installed, and check-ins return at once, so the service behaves exactly as
before (`make cvm-rehearsal` runs with it empty and asserts `"sentry_enabled":false`). A malformed
DSN stops `topup run` at startup; `preflight.sh` checks the format without printing it.

- **Release and environment.** The release is the image digest (`sha256:...`) of `TOPUP_IMAGE`,
  which `render-compose.sh` pins into the topup service's environment; the environment is the
  compose's literal `SENTRY_ENVIRONMENT` (`staging`; a production compose sets `production`). Both
  are attested with the compose. A restore-check instance (`TOPUP_SERVICE_ENABLED=read-only`,
  [RESTORE.md](RESTORE.md)) reports as `<environment>-restore` (`staging-restore`) and runs no
  loop, so it never checks in to a Crons monitor or raises an alert scoped to the live
  environment.
- **Events.** Every `ERROR` log line and every panic (the panic hook flushes before the release
  profile aborts) is an event, grouped by its constant message. `WARN` lines tagged `tags.alert`
  are events too; other `INFO` and `WARN` lines are only breadcrumbs of the next event. At most one
  event per issue is sent every 10 minutes, since failing loops retry every few seconds. The
  service filters these itself: the organization's Sentry plan ignores per-key rate limits (the
  client key's `rateLimit` stays `null`), and its event quota is shared with other Phala projects.
- **Alerts.** A line tagged `tags.alert` carries the Prometheus alert name
  ([runbooks index](runbooks/README.md#alert-and-symptom-index)), is fingerprinted by that name
  and its other `tags.*` (route, state, check, chain, scope; never a deposit id), and gets a
  `runbook` tag linking the runbook: `TopupDepositStateAgeExceeded`,
  `TopupReconciliationMismatch`, `TopupLockExpiryFailing`, `TopupLockExposureDrift`,
  `TopupLockExposureNearCap`, `TopupUnsupportedInflows`, `OperatorRoleMissing`, and the other
  flusher alerts by variant name (`Reverted`, `IsolatedAddress`, `MissingConsumedReceipt`,
  `PlanningExcluded`, `FeeCapReached`, `NativeBalance`).
- **Data.** An event holds what the production JSON log line holds, behind the same INFO ceiling
  and silenced provider-transport targets (no RPC URL), minus `account_id`, a product's end-user
  identifier. Internal UUIDs, chain ids, routes, and on-chain addresses stay: the runbooks need
  them, and they are public on chain. Spans are not sent, there is no HTTP integration (no request
  body, header, URL, or client IP), and `send_default_pii` is off.
- **Crons.** Each loop that must keep running checks in with its monitor configuration, which
  creates or updates the monitor (upsert); a monitor checks in at most once a minute. A monitor
  exists only after its first check-in.

  | Monitor slug | Check-in | Schedule | Margin | Replaces |
  |---|---|---|---|---|
  | `topup-scanner-<chain_id>` (`topup-scanner-11155111`) | `ok` after each successful finalized scan | every 1 min | 5 min | `TopupScannerLag`, `TopupLoopStopped{loop="scanner"}` |
  | `topup-pump-<n>` (`topup-pump-0`) | `ok` at each pump iteration (a step may take 4 min) | every 1 min | 5 min | `TopupLoopStopped{loop="pump"}` |
  | `topup-outbox-<n>` (`topup-outbox-0`) | `ok` at each webhook delivery poll | every 1 min | 5 min | `TopupLoopStopped{loop="outbox"}` |
  | `topup-lock-expiry` | `ok` after each successful rate-lock expiry scan | every 1 min | 5 min | `TopupLockExpiryFailing`, `TopupLoopStopped{loop="lock_expiry"}` |
  | `topup-reconciler` | `ok` after a complete round, `error` after a round with failed checks | every 10 min | 10 min | `TopupLoopStopped{loop="reconciler"}` |
  | `topup-backup` | `ok` while the WAL-G marker is at most 120 s old, else `error`; 3 errors in a row open an issue | every 1 min | 2 min | `TopupBackupTooOld` |
  | `topup-flush-<route>` (`topup-flush-phala-cloud-sepolia-pha-usd`) | `ok` after scheduled planning, `error` when planning failed or the operator lacks `OPERATOR_ROLE` | the route's `flush.schedule` (`0 */6 * * *`), UTC | 15 min | `TopupLoopStopped{loop="flusher"}` |

- **Uptime.** `/healthz` at the gateway is watched by a Sentry Uptime monitor (below).
- **Egress.** With a DSN, `topup` sends HTTPS (443) to the DSN's ingest host
  (`o<org>.ingest.<region>.sentry.io`); add it to the egress allow-list
  ([Attestation, ingress, and egress](#attestation-ingress-and-egress)).

`TopupOperatorGasReserveLow` has no producer yet (`producer_enabled="false"`), so neither
Prometheus nor Sentry can raise it; `/metrics` stays the standard local surface.

**One-time setup (HUMAN-ONLY, Sentry project admin).** The Crons monitors need none. Verify every
step against the Sentry UI; nothing here is in the repository.

1. Project **Settings > Security & Privacy**: keep *Data Scrubber* and *Use Default Scrubbers* on,
   and turn *Prevent Storing of IP Addresses* on.
2. **Alerts**: an alert on `crypto-topup-service` for the environments `staging` and `production`
   (not `*-restore`, which only restore and drill instances use) that notifies the on-call
   owner when an issue is created or moves from resolved back to unresolved, without a level
   filter (alert lines are `warning` events). After steps 3 and 4, open the alert's details and
   confirm that the Crons monitors and the Uptime monitor are listed as connected monitors;
   connect any that are missing.
3. **Uptime monitor** (Monitors > Uptime > Add): `GET https://<app_id>-8080.<gateway base
   domain>/healthz` (the `TOPUP_PUBLIC_ORIGIN` of the run summary plus `/healthz`), interval
   1 minute, timeout 10 seconds, environment `staging`, project `crypto-topup-service`. Sentry
   documents no API for creating uptime monitors, so use the UI.
4. **Seal the DSN.** Copy the project's DSN (Settings > Client Keys) into the owner's sealed env
   file as `SENTRY_DSN` and seal it with `phala envs update` from the owner's machine as in
   [Deploy](#deploy) step 5. Within a minute of the restart the Crons page lists the monitors
   above.

## Service startup checks

`topup run` refuses to start until every RPC provider of every route shows the route's factory,
`implementation()`, `treasury()`, `factory()`, `addressOf(sample salt)`, and the recorded
contract code (architecture §4, §14). The check runs before the service touches the database, so
an outage of any single configured provider blocks restarts by design; a running service is not
affected. Restore the provider or wait for it; do not remove it from the route to get past the
check, since the route is attested.

Two routes that name the same product must agree on `destination.settlement_url` and
`destination.product_kid`, or startup fails. The `restore-check` service pins one route
file (`phala-cloud-sepolia-pha.yaml`) in its entrypoint, so adding a second route or product also
requires adding that route to `restore-check` and to the `topup run` command in the compose.

## Database credentials

PostgreSQL runs inside the CVM, so its passwords are derived there like every other key, never
supplied. The `keys` service (`topup keys`, the only container besides `topup` with the dstack
socket) derives the WAL-G keys and, as the lowercase hex of `get_key("db/owner/v1")` and
`get_key("db/app/v1")` (secp256k1), the owner and application passwords. It writes them to three
tmpfs volumes and holds them mounted; its `--check` healthcheck gates PostgreSQL. Each service
mounts, read-only, only the volumes it needs:

| Volume (path) | Files | Mounted by |
|---|---|---|
| `walg_key` (`/run/wal-g`) | `backup.key`, `backup-vN.key` | `postgres`, `backup`, `restore` |
| `db_owner` (`/run/db-owner`) | `postgres.password` (`POSTGRES_PASSWORD_FILE`), `postgres.pgpass` | `postgres`, `migrate`, `backup`, `restore-check` |
| `db_app` (`/run/db-app`) | `topup_service.pgpass` | `postgres` (the init script reads its password field), `topup`, `heartbeat` |

URLs carry no password (`postgres://topup_service@postgres:5432/topup`); sqlx, `psql`, and WAL-G
read libpq's standard `PGPASSFILE`. Every file is mode `0600`, owned by uid 999, which every
database client runs as; isolation is by mount, and `deploy/validate-compose.sh` requires that
`topup` and `heartbeat` mount neither `db_owner` nor `walg_key`. The same app id derives the same
passwords, so a replacement or restored CVM logs in unchanged
([RESTORE.md](RESTORE.md#backup-key-and-metadata)). The version is part of the path; rotating
means an `ALTER ROLE` to a `db/*/v2` value inside the CVM and a new compose.

## Backup age marker contract

After a successful `walg-wal-push` (key-versioned WAL upload and metadata) or `walg-base-backup`,
`walg-cron` atomically writes the current Unix timestamp as decimal ASCII plus a newline to
`TOPUP_BACKUP_TIMESTAMP_FILE` with mode `0644`; the marker is operational metadata and contains no
secret. PostgreSQL uses `walg-cron wal-push %p` as its archive command, so the 60-second
`archive_timeout` drives the two-minute alert. `archive_timeout` only switches a segment that
contains new WAL; the `heartbeat` service commits one row every minute, so an idle database still
archives a segment and refreshes the marker every minute. The `backup` service requests one
`CHECKPOINT` per postmaster start because PostgreSQL 15+ (re-checked on 18.6) otherwise ignores `archive_timeout` until
the checkpointer first wakes, up to `checkpoint_timeout` after startup. The measured
compose shares `/run/topup-observability/last-backup-unix-seconds` read-write with `postgres` and
`backup`, and read-only with `topup`. The service exports the marker value as
`topup_backup_last_success_unixtime_seconds`; a missing or malformed marker exports zero so the
PromQL age calculation fails closed. Secret files remain mode `0600` and must not be written into
the observability volume.

## Build and publish images

Images are built and published only by CI, by the
[Release images](../.github/workflows/release-images.yml) workflow; never push them from a
workstation. It runs only on `workflow_dispatch` and publishes only from `main`; a dispatch on
any other ref fails. All images get the same tag, `sha-<12-hex commit>` plus an optional
suffix, and `SOURCE_DATE_EPOCH` is the commit time:

- `ghcr.io/phala-network/crypto-topup:<tag>`: [verify-image.sh](verify-image.sh) with
  `PUBLISH_IMAGE` set performs two clean BuildKit OCI exports for `linux/amd64`, with provenance
  and SBOM attachments disabled and `rewrite-timestamp=true`, and fails unless their OCI manifest
  and config digests match. It then builds and pushes a third time and fails unless the registry's
  platform manifest and config digests equal the verified local ones. This proves repeatability on
  the CI builder and platform, not cross-builder or cross-architecture identity.
- `ghcr.io/phala-network/postgres-walg:<tag>` from
  [Dockerfile.postgres-walg](Dockerfile.postgres-walg): apt and dpkg record wall-clock times, so
  it is not bit-for-bit reproducible. It is built and pushed once; the registry tag must resolve
  to the digest BuildKit pushed, and that digest, pulled from the registry, must run
  `wal-g --version`. Making this image reproducible (removing the apt and dpkg logs and caches,
  then the same two-build check) is a follow-up.
- `ghcr.io/phala-network/crypto-topup-reference-product:<tag>` from
  [Dockerfile.reference-product](Dockerfile.reference-product): the same `verify-image.sh`
  two-build, push, and read-back check as `crypto-topup` (with `DOCKERFILE` set), then the
  digest, pulled from the registry, must run `--help`.

Run the workflow on `main` from the Actions tab, or with
`gh workflow run release-images.yml --ref main`. The job summary and the `images.json` artifact
hold the manifest references (`TOPUP_IMAGE`, `POSTGRES_WALG_IMAGE`, `PRODUCT_IMAGE`): pass the
first two to Deploy staging as `topup_image` and `postgres_walg_image`, and `PRODUCT_IMAGE` to
Deploy staging product as `product_image`. For a local render or preflight:

```sh
images=$(mktemp -d)
gh run download <run-id> --repo Phala-Network/crypto-topup-service --name images-<tag> --dir "$images"
eval "$(jq -r 'to_entries[] | "export \(.key)=\(.value | @sh)"' "$images/images.json")"
```

Deploy those `repository@sha256:<platform manifest digest>` references, never a tag, and do not
assume a registry index digest equals its platform manifest digest: registry-added indexes or
attestations can change the outer digest while the child manifest and config stay identical. No
build provenance attestation is published: GitHub artifact attestations need GitHub Enterprise
Cloud for private repositories.

**HUMAN-ONLY, one-time, package admin:** CVMs pull without registry credentials, so both packages
must be public; never add registry credentials to a CVM. GitHub's REST API cannot change a
container package's visibility. Making a package public is irreversible: it cannot be made private
again. Prerequisite: the organization must allow public container packages (organization
Settings, Packages, Package creation, with Public enabled for containers); otherwise the Public
option is unavailable. After the first publish, for `crypto-topup`, `postgres-walg`, and
`crypto-topup-reference-product`: open
the package under the organization's Packages tab, then Package settings, Danger Zone, Change
visibility, Public, and confirm with the package name. Deploy staging's preflight pulls both
digests anonymously (Deploy staging product's preflight its own) and fails while a package is
still private.

Developer check, no push and no credentials: the same two-build comparison runs locally with

```sh
deploy/verify-image.sh   # or make verify-image
```

Deploy staging renders literal, nonzero image digests and the settings into the compose; secret
values remain `${NAME:-}` references ([Attested settings](#attested-settings)). To review a render
locally, export the settings of the `staging` Environment variables and:

```sh
export TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:<64-hex-digest>
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:<64-hex-digest>
deploy/render-compose.sh > "$(mktemp)"
```

Both database URLs are encrypted, but the compose enforces their separate consumers.

## Authoritative manifest and hash

CLI 1.1.22 builds app-compose internally from the exact YAML bytes, privacy/storage flags, and env
names passed to `phala deploy`. It derives `allowed_envs` from `-e`; it does not submit this repo's
`app-compose.example.json`, and it does not submit that old template's `port_policy`. The CLI has no
command that prints the complete app-compose before initial provisioning.

Therefore:

- Never authorize a hash produced from `render-app-compose.sh` before deployment. It is only useful
  for review and tests.
- For an existing on-chain-KMS CVM, `phala deploy --prepare-only --json` returns the authoritative
  `compose_hash` and a commit token bound to that prepared update. Approve that hash, then commit the
  same token against the same `--cvm-id`.
- For first-time provisioning, CLI 1.1.22 does not stop at `--prepare-only`. It provisions the CVM,
  deploys its app authorization contract, registers the initial hash, and commits the CVM. Treat the
  first CVM as staging, read back its attested `compose_file`, and verify it before enabling a route.

## A. First-time provisioning

Staging is provisioned by the Deploy staging workflow with Phala Cloud's KMS (see the checklist).
This section and section B are the production path, which has not been exercised yet: on-chain
KMS on Base mainnet, chain ID 8453, with the app authorization contract owned by the Finance Safe.
Its env file (the `staging.env.example` names with the production secrets) and rendered compose
(`render-compose.sh` with the production settings, [Attested settings](#attested-settings), and
`TOPUP_PUBLIC_ORIGIN=https://pending.invalid`) are prepared like the workflow's and never
committed. The asset chain in the route is independent of
the KMS control plane. On 2026-09-22, CLI 1.1.22 reported the Base KMS contract as
`0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C`; re-query it and confirm the selected contract has
allowed devices and an allowed production OS image. **HUMAN-ONLY, Phala Cloud credentials required:** authenticate before querying the
operator's available KMS control plane:

```sh
npx --yes phala@1.1.22 login --no-open
npx --yes phala@1.1.22 kms base --json > base-kms.json
jq '{chain_id, contracts: [.contracts[] | {contract_address, devices, os_images}]}' base-kms.json
export KMS_CONTRACT=0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C
```

**HUMAN-ONLY, Phala Cloud, Base RPC, and provisioner-key credentials required:** authenticate
and load `PRIVATE_KEY` and `ETH_RPC_URL` from the operator's secret manager; the CLI reads both
from the environment, so neither appears on a command line. Do not add `--prepare-only`; it does
not halt the create path in this CLI version. Run [preflight.sh](preflight.sh) first with the
default `--kms base`, which also checks the KMS contract's devices and OS images, and pass the
same `$OS_IMAGE` (`dstack-0.5.9`, [OS image](#os-image)): without `--image` the platform picks
the OS image, and a different dstack release derives different keys:

```sh
deploy/preflight.sh --env .env.production --compose deploy/docker-compose.production.yml \
  --workspace "$PHALA_WORKSPACE" --os-image "$OS_IMAGE" --kms-contract "$KMS_CONTRACT"
```

Phala CLI 1.1.22 writes `Provisioning CVM ...` to stdout before writing the pretty-printed JSON
object, and its deploy command has no separate output-file option. Preserve the complete stdout for
the deployment record, then extract from the first JSON object line and validate it before reading
any fields. See the pinned
[create handler](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts#L750-L752),
[JSON response](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts#L869-L887),
and [deploy options](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/command.ts#L373-L377).

```sh
npx --yes phala@1.1.22 deploy --json \
  --name crypto-topup \
  --compose deploy/docker-compose.production.yml \
  -e .env.production \
  --instance-type tdx.medium \
  --fs ext4 \
  --kms base \
  --kms-contract "$KMS_CONTRACT" \
  --image "$OS_IMAGE" \
  --no-dev-os \
  --no-public-logs \
  --no-public-sysinfo \
  --public-tcbinfo \
  --secure-time \
  --wait > provision.raw
sed -n '/^{/,$p' provision.raw > provision.json
jq -e . provision.json >/dev/null
CVM_ID=$(jq -er '.vm_uuid' provision.json) || exit 1
APP_ID=$(jq -er '.app_id' provision.json) || exit 1
export CVM_ID APP_ID
case "$APP_ID" in 0x*) export APP_AUTH_CONTRACT="$APP_ID" ;; *) export APP_AUTH_CONTRACT="0x$APP_ID" ;; esac
```

The CLI deploys a new `DstackApp` authorization contract and returns its address as `app_id`. Its
initializer registers the initial compose hash and device, and the provisioner EOA initially owns
the contract.

**HUMAN-ONLY, provisioner key and Finance Safe required:** transfer ownership using the
`Ownable2Step` flow, then have the Finance Safe execute `acceptOwnership()` on Base:

```sh
cast send "$APP_AUTH_CONTRACT" 'transferOwnership(address)' "$FINANCE_SAFE" \
  --rpc-url "$ETH_RPC_URL" --private-key "$PRIVATE_KEY"
cast calldata 'acceptOwnership()'
cast call "$APP_AUTH_CONTRACT" 'owner()(address)' --rpc-url "$ETH_RPC_URL"
```

The final owner must equal `$FINANCE_SAFE`. The calldata printed above is the Safe transaction data;
do not remove the provisioner's access until the Safe acceptance is final.

Read back and verify the artifact that was actually deployed:

```sh
npx --yes phala@1.1.22 cvms get "$CVM_ID" --json > cvm.json
npx --yes phala@1.1.22 cvms attestation "$CVM_ID" --json > attestation.json
APP_ID=$(jq -er '.app_id' cvm.json) || exit 1
GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json) || exit 1
curl -fsS "https://${APP_ID#0x}-8090.$GATEWAY_DOMAIN/prpc/Info" > info.json
deploy/verify-attestation.sh \
  attestation.json info.json "$APP_ID" deploy/docker-compose.production.yml
```

If the attestation is unavailable or its `compose_file` cannot be read back, stop. Do not enable the
route or represent the locally previewed manifest as the deployed artifact.

### Public origin follow-up

Deploy staging performs this follow-up for a staging CVM. A production CVM boots with the
provisional `TOPUP_PUBLIC_ORIGIN=https://pending.invalid`, which passes startup validation, but
every request signed for the real URL is rejected until the origin is updated; during this window only the admin key exists, and no product credentials may
be issued. Derive the gateway URL of port 8080 from the provisioned app, confirm it serves the
service, and only then render it into the compose. CLI 1.1.22 `cvms get --json` (API
2026-06-23) reports the gateway domain as `gateway.base_domain`; every extraction below stops on
a missing field instead of writing a URL containing `null`:

```sh
GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json) || exit 1
TOPUP_PUBLIC_ORIGIN="https://${APP_ID#0x}-8080.$GATEWAY_DOMAIN"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/healthz" || exit 1
export TOPUP_PUBLIC_ORIGIN
deploy/render-compose.sh > deploy/docker-compose.production.yml
deploy/preflight.sh --env .env.production --compose deploy/docker-compose.production.yml --offline
```

The origin is attested, so this is a new compose hash: apply it as an upgrade with
[B. Upgrade an existing CVM](#b-upgrade-an-existing-cvm) (Finance Safe approval of the returned
hash).

When `topup` is running again, repeat the read-back above (the compose hash must be the approved
one), confirm that a correctly signed administrative request is accepted (see
[Attestation, ingress, and egress](#attestation-ingress-and-egress)), and request an attestation
through the public URL:

```sh
export NONCE="$(openssl rand -hex 32)"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/attestation?nonce=$NONCE" > public-attestation.json
jq -e --arg nonce "$NONCE" '.keyid == "settlement/v1" and (.quote | length > 0)' \
  public-attestation.json
```

Check the `report_data` binding and verify the quote as described in
[Attestation, ingress, and egress](#attestation-ingress-and-egress) before any product pins the
settlement key, then grant and fund the flusher operator it reports
([Flusher operator](#flusher-operator)).

## B. Upgrade an existing CVM

Staging upgrades run Deploy staging with `mode: upgrade`. For an on-chain-KMS (production) CVM, use
the same literal compose and env file for prepare and commit. The commit token binds the update,
so any compose/env change requires a new prepare. A changed setting
([Attested settings](#attested-settings)) is a re-rendered compose and so an upgrade like this one.

**HUMAN-ONLY, Phala Cloud and encrypted-env credentials required:** prepare against the existing CVM:

```sh
export CVM_ID=<existing-cvm-id>
npx --yes phala@1.1.22 deploy --json \
  --cvm-id "$CVM_ID" \
  --compose deploy/docker-compose.production.yml \
  -e .env.production \
  --prepare-only > prepare.json
COMPOSE_HASH=$(jq -er '.compose_hash' prepare.json) || exit 1
COMMIT_TOKEN=$(jq -er '.commit_token' prepare.json) || exit 1
APP_ID=$(jq -er '.app_id' prepare.json) || exit 1
export COMPOSE_HASH COMMIT_TOKEN APP_ID
case "$APP_ID" in 0x*) export APP_AUTH_CONTRACT="$APP_ID" ;; *) export APP_AUTH_CONTRACT="0x$APP_ID" ;; esac
```

Confirm `prepare_only` is true and review `onchain_status`. **HUMAN-ONLY, Finance Safe required:**
approve the exact returned hash on the Base `DstackApp`, wait for finality, and record the Safe
transaction hash:

```sh
jq '{prepare_only, compose_hash, app_id, device_id, chain_id, onchain_status}' prepare.json
cast calldata 'addComposeHash(bytes32)' "$COMPOSE_HASH"
cast call "$APP_AUTH_CONTRACT" 'allowedComposeHashes(bytes32)(bool)' "$COMPOSE_HASH" \
  --rpc-url "$ETH_RPC_URL"
export ALLOWLIST_TRANSACTION_HASH=<final-safe-transaction-hash>
```

The call must return `true`. If `onchain_status.device_id_allowed` is false, the Safe must also
execute `addDevice(bytes32)` for the returned `device_id` before commit.

**HUMAN-ONLY, Phala Cloud credentials required:** commit the same prepared update to the same target:

```sh
npx --yes phala@1.1.22 deploy --json \
  --cvm-id "$CVM_ID" \
  --commit \
  --token "$COMMIT_TOKEN" \
  --compose-hash "$COMPOSE_HASH" \
  --transaction-hash "$ALLOWLIST_TRANSACTION_HASH" \
  --wait > commit.json
npx --yes phala@1.1.22 cvms get "$CVM_ID" --json > cvm.json
npx --yes phala@1.1.22 cvms attestation "$CVM_ID" --json > attestation.json
APP_ID=$(jq -er '.app_id' cvm.json) || exit 1
GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json) || exit 1
curl -fsS "https://${APP_ID#0x}-8090.$GATEWAY_DOMAIN/prpc/Info" > info.json
deploy/verify-attestation.sh \
  attestation.json info.json "$APP_ID" deploy/docker-compose.production.yml
```

After the observation window, **HUMAN-ONLY, Finance Safe required:** remove the old hash with
`removeComposeHash(bytes32)`. Rollback is another upgrade: prepare the retained old compose, approve
its returned hash if necessary, commit its token, and repeat attestation checks. Never roll a schema
back destructively; use a forward repair migration.

## Attestation, ingress, and egress

Request an application-bound quote with a fresh nonce:

```sh
export NONCE="$(openssl rand -hex 32)"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/attestation?nonce=$NONCE" > public-attestation.json
```

Production images have no logs or SSH, so this public endpoint is the only source of the
settlement key and the flusher operator addresses.

**HUMAN-ONLY, verifier approval required:** `quote` is the guest agent's versioned attestation
(quote, event log, and vm_config), so [dstack-verifier.sh](dstack-verifier.sh) verifies it as is:
quote and TCB, the RTMR3 replay, and the OS image. Its app id and compose hash must be the CVM's
(`cvm.json` and the `attestation.json` read back above) and its 64-byte report data the 32-byte
`report_data` zero-padded:

```sh
jq '{quote: null, attestation: .quote}' public-attestation.json |
  deploy/dstack-verifier.sh > public-verification.json
jq -e --arg app "$(jq -r '.app_id | ltrimstr("0x") | ascii_downcase' cvm.json)" \
  --arg compose "$(jq -j '.compose_file' attestation.json | sha256sum | cut -d' ' -f1)" \
  --arg report_data "$(jq -r '.report_data' public-attestation.json)" '
  .details.tcb_status == "UpToDate"
  and .details.app_info.app_id == $app
  and .details.app_info.compose_hash == $compose
  and .details.report_data == $report_data + ("0" * 64)
' public-verification.json
```

Then check that `report_data` binds the fresh nonce, the returned `settlement/v1` public key, and
every returned operator:

```sh
python3 - "$NONCE" public-attestation.json <<'PY'
import hashlib, json, sys
nonce, body = bytes.fromhex(sys.argv[1]), json.load(open(sys.argv[2]))
data = nonce + bytes.fromhex(body["settlement_pubkey"])
for op in body["operators"]:
    assert op["keyid"] == f"operator/v{op['operator_key_version']}", op
    data += op["chain_id"].to_bytes(8, "big") + op["operator_key_version"].to_bytes(4, "big")
    data += bytes.fromhex(op["address"].removeprefix("0x"))
assert hashlib.sha256(data).hexdigest() == body["report_data"], "report_data does not bind the keys"
print("ok: report_data binds the nonce, settlement key, and operators")
PY
```

The Python SDK's `TopupClient.attestation` performs the same check
(`topup_sdk.verify_attestation_binding`). Architecture §14 defines the construction; with no
operators it is `sha256(nonce ‖ settlement_pubkey)`.

CLI 1.1.22 does not submit `port_policy`. Ingress is therefore verified after deployment from the
attested compose and the live gateway:

```sh
npx --yes phala@1.1.22 runtime-config "$CVM_ID" --json > runtime-config.json
jq '{hostname, default_gateway_domain}' runtime-config.json
jq -r '.compose_file' attestation.json | jq -r '.docker_compose_file' \
  | docker compose -f - config --format json \
  | jq '.services | with_entries(.value = (.value.ports // []))'
```

The result must expose only `topup` TCP port 8080; `postgres`, `migrate`, and `backup` must expose no
ports. **HUMAN-ONLY, external network access required:** derive the port-8080 TLS hostname from the
returned gateway domain, request `/openapi.json`, validate its certificate, and confirm connection
attempts to PostgreSQL are rejected. Record the exact URL and results in the deployment ticket.
The encrypted `TOPUP_PUBLIC_ORIGIN` must be exactly this URL's scheme and authority (no path):
the service verifies every signed `@target-uri` against it and ignores `Host` and
`X-Forwarded-*`. Confirm it before issuing product credentials; a correctly signed admin request
returning `401` usually means the two differ.

dstack app-compose has no hostname egress allow-list. **HUMAN-ONLY, cloud network authority
required:** restrict outbound access to the two RPC hosts, configured price-source hosts, object
storage host, attested product settlement host, the Sentry ingest host of `SENTRY_DSN` when it is
set ([Sentry](#sentry)), DNS, and required Phala/dstack platform endpoints.
Record resolved hostnames, ports, and enforcement rules. Do not treat `pre_launch_script` as the
firewall boundary because it runs after Docker startup.

Before enabling a route, also confirm real route addresses validate without template mode, both RPC
providers agree at `finalized`, Safe/factory/implementation/CREATE2 checks pass, migrations completed,
WAL archiving is current, the product pins the attested settlement key, the attested flusher
operator of each chain holds `OPERATOR_ROLE` and has gas ([Flusher operator](#flusher-operator)),
pilot limits are approved, and a restore drill per [RESTORE.md](RESTORE.md) has passed.

### Flusher operator

`operators` lists, per configured chain, the key the flusher signs `flush` with: `chain_id`,
the current routes' `operator_key_version`, `keyid` (`operator/v{n}`), and `address`. The address
is public and can only flush forwarders to the immutable treasury, but it needs `OPERATOR_ROLE` on
the chain's factory and native gas before any flush is sent. Use only an address from a response
whose quote and binding were verified in
[Attestation, ingress, and egress](#attestation-ingress-and-egress):

```sh
export CHAIN_ID=11155111 FACTORY=<chain factory> ETH_RPC_URL=<chain RPC>
export OPERATOR_ADDRESS="$(jq -er --argjson chain "$CHAIN_ID" \
  '.operators[] | select(.chain_id == $chain) | .address' public-attestation.json)"
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
```

1. **HUMAN-ONLY, admin Safe required:** execute the printed `grantRole` calldata on `$FACTORY`,
   wait for finality, and confirm
   `cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"`
   returns `true`.
2. **HUMAN-ONLY, gas funds required:** fund `$OPERATOR_ADDRESS` with native gas on the chain
   ([runbooks/gas-refill.md](runbooks/gas-refill.md)) and check `cast balance "$OPERATOR_ADDRESS"`.

A flusher whose operator lacks the role plans and sends nothing, raises an `OperatorRoleMissing`
alert at every maintenance interval, and resumes by itself once the role is granted.
`make cvm-rehearsal` exercises this path: it reads the address from `/v1/attestation`, checks it
against `topup attest --route`, grants it through the mock Safe, funds it, and waits for the
flusher to report the role.

To rotate the operator key:

1. Bump `operator_key_version` in the attested chain and route files, then upgrade as in
   [B. Upgrade an existing CVM](#b-upgrade-an-existing-cvm) (new compose hash, allow-list, deploy). Every current route on one
   chain must share the version. Unsigned plans are re-bound to the new operator and in-flight
   flushes of the old one keep confirming; new flushes wait for the grant.
2. Request a fresh attestation, verify it, and read the new `operators` entry.
3. Grant `OPERATOR_ROLE` to the new address and fund it, as in steps 1 and 2 above; the flusher
   resumes by itself.
4. **HUMAN-ONLY, admin Safe required:** once no flush of the old operator is in flight, revoke
   its `OPERATOR_ROLE`.

Where a shell is available (staging with SSH, local stacks), `topup attest --nonce <hex> --route
<file>` prints the same `settlement_pubkey`, `operators`, and `report_data` as the endpoint for
those route files, plus `operator_keyid` and `operator_address` for `--operator-key-version`
(default 1), which is not bound into the report data. dstack derives keys from the application
identity rather than the compose hash, so this previews the next operator address and lets the
Safe grant it before the bump, without a pause in flushing.

## Product credentials

`POST /v1/admin/products` is the only way to issue a product: CVMs have no SSH, logs, or database
access. It stores the product's slug, ed25519 request-verification public key, and webhook URL,
and writes an `audit` row (`actor = admin:<TOPUP_ADMIN_KID>`, `action = product.issue`). The key
id and settlement URL are not part of it: the attested route's `destination.product_kid` and
`destination.settlement_url` are their only source, so the slug must be named by a route the
service loaded, and the route change comes first.

The body is `{"slug", "public_key", "webhook_url"}`: `slug` matches
`^[a-z0-9][a-z0-9-]{0,62}$`, `public_key` is the standard base64 of the 32-byte key the integrator
printed with `topup-sdk keygen` (never the seed), and `webhook_url` is an absolute `https` URL
without credentials (`http` is accepted only when the route's settlement URL is itself `http`,
which only local stacks use). Responses: `200` with the product, also when the slug is already
registered with the same values; `409 conflict` when it is registered with a different key or
webhook URL; `400` for an invalid field or a slug no loaded route names.

**HUMAN-ONLY, admin key holder:** complete
[Attestation, ingress, and egress](#attestation-ingress-and-egress) first. The signing helper takes
a PEM key; convert the `topup-sdk keygen` seed file once, then sign the exact body and send it:

```sh
(umask 077 && { printf '302e020100300506032b657004220420'; tr -d '\n' < admin.seed; } |
  xxd -r -p | openssl pkey -inform DER -out admin.pem)
export ADMIN_KEY_FILE=admin.pem ADMIN_KEY_ID=admin/v1   # the CVM's TOPUP_ADMIN_KID
jq -cjn --arg public_key '<base64 from the integrator>' \
  '{slug: "phala-cloud", public_key: $public_key,
    webhook_url: "https://product.example/topup/webhooks"}' > /tmp/topup-product.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST \
  "$TOPUP_PUBLIC_ORIGIN/v1/admin/products" /tmp/topup-product.json \
  "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' \
  -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
  --data-binary @/tmp/topup-product.json "$TOPUP_PUBLIC_ORIGIN/v1/admin/products"
```

Changing a registered product's public key or webhook URL is not supported yet (the endpoint
answers `409`); a new key also needs a new key id, which is a new route version. Changing its
settlement URL is a new route version.

## Staging reference product

Staging settles deposits against a second, small CVM running the repository's reference
product, [sdk/examples/phala_cloud_integration.py](../sdk/examples/phala_cloud_integration.py),
as the `phala-cloud` product: `serve` mode is the settlement endpoint (all six product
obligations), the webhook receiver, and the product's own account API, with its ledger in SQLite on
the CVM's `ledger` volume; `deposit` mode, run from an operator's machine, plays a Phala Cloud user.
The product holds the product signing key and calls topup on the user's behalf; the driver
signs its account API requests with a separate driver key (`driver/v1`) that cannot sign topup
requests.

| Piece | Where |
|---|---|
| Image | `ghcr.io/phala-network/crypto-topup-reference-product` ([Dockerfile.reference-product](Dockerfile.reference-product)), published by Release images |
| Compose | [product/docker-compose.yml](product/docker-compose.yml): one service, port 8089, the staging route's addresses inline, no capabilities; rendered by [product/render-compose.sh](product/render-compose.sh) |
| Env names | [product/staging.env.example](product/staging.env.example): only `PRODUCT_SEED`, the secret |
| Workflow | [Deploy staging product](../.github/workflows/deploy-staging-product.yml): same CLI (1.1.22), `--kms phala`, the approved production OS image, `tdx.small`, no public logs or sysinfo, preflight ([product/preflight.sh](product/preflight.sh)), attestation verified with [verify-attestation.sh](verify-attestation.sh) |

The product's public configuration is attested: the workflow renders it into the product
config inside the compose, so it is part of the compose hash. `TOPUP_ORIGIN` is read by the
workflow from the topup CVM (`vars.STAGING_CVM_ID`) and `PRODUCT_PUBLIC_URL` is the product CVM's
own gateway URL; `PRODUCT_RPC_URL` (the product's own Sepolia RPC, preferably a provider topup does
not use) and `PRODUCT_DRIVER_PUBLIC_KEY` are `staging` Environment variables. The sealed env holds
only `PRODUCT_SEED`. At startup the product fetches `TOPUP_ORIGIN/v1/attestation` with a fresh
nonce, checks it with `topup_sdk.verify_attestation_binding`, and pins the `settlement/v1` key.
GitHub holds only `PHALA_CLOUD_API_KEY`.

`PRODUCT_RPC_URL` is public in the attested compose (and the run's artifact): use a keyless public
Sepolia RPC. A keyed URL would publish its key. The preflight requires the RPC's `finalized` block
to be within 64 blocks of topup's providers (`TOPUP_RPC_PROVIDER_A_URL` and `_B_URL`): the product
defers (503) every settlement until its own RPC has finalized the deposit's block, so a lagging
RPC stalls all of them.

**Changing a setting** (the RPC, the driver key, or topup's origin): set the `staging` Environment
variable if it is one, then run Deploy staging product in mode `upgrade` with the current
`product_image`. Never change these with `phala envs update`: Compose recreates a container only
when its service definition changes, and a restart keeps the old config file. The renderer labels
the service with the digest of the rendered compose, so every rendered change recreates the
container.

A product CVM provisioned before the settings were attested has all five names in its
`allowed_envs`. Once: seal `.env.product` holding only `PRODUCT_SEED` (step 4; this sets
`allowed_envs` to that name), then run Deploy staging product in mode `upgrade`. Until the upgrade
the old compose can read empty settings.

### End-to-end order

Each step is **HUMAN-ONLY** unless marked as a workflow run; nothing is deployed from a laptop.

1. **Keys, on the owner's machine** (mode-0600 files, never committed or sent anywhere):

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid phala-cloud/v1 --seed-out ~/staging/product.seed
   uv run --locked topup-sdk keygen --keyid driver/v1 --seed-out ~/staging/driver.seed
   ```

   Set the `staging` Environment variables `PRODUCT_DRIVER_PUBLIC_KEY` (the driver's printed
   `public_key`) and `PRODUCT_RPC_URL` (a keyless public Sepolia RPC).
2. **Release** (workflow): run Release images on `main`; make the new
   `crypto-topup-reference-product` package public once
   ([Build and publish images](#build-and-publish-images)).
3. **Provision the product CVM** (workflow): Deploy staging product, mode `provision`,
   `product_image` = `PRODUCT_IMAGE` from the Release images summary. Then set the `staging`
   Environment variable `STAGING_PRODUCT_CVM_ID` to the printed CVM id. The run upgrades the new
   CVM once more to a compose rendered with its gateway URL. The summary lists the public,
   settlement, and webhook URLs (`https://<app-id>-8089.<gateway domain>`); later runs use mode
   `upgrade`, which keeps the sealed env.
4. **Seal the product seed:** write `.env.product` (mode 0600) with the single line
   `PRODUCT_SEED=<the hex seed in ~/staging/product.seed>`, then run the two commands the summary
   prints (`deploy/product/preflight.sh ... --offline` and
   `phala envs update <cvm-id> -e .env.product`). Until then the account API answers 503.
5. **Register the product in topup** with the admin-signed `POST /v1/admin/products`, exactly as
   [Product credentials](#product-credentials) shows: slug `phala-cloud`, `public_key` the
   `phala-cloud/v1` keygen output, and `webhook_url` `<product URL>/webhooks`. The committed
   staging route already names `phala-cloud` (with the placeholder settlement URL), so this can
   precede step 6; a repeat with the same values answers `200`.
6. **Point the route at the product:** a reviewed PR sets `settlement_url` to
   `<product URL>/settlements` in `deploy/config/routes/phala-cloud-sepolia-pha.yaml` and in the
   inline copy in `deploy/docker-compose.yml` (`deploy/validate-compose.sh` compares them). This
   is attested route content, so it changes topup's compose hash: after merging, run Deploy
   staging in mode `upgrade` ([B. Upgrade an existing CVM](#b-upgrade-an-existing-cvm)).
7. **Run the deposit driver** from the operator's machine. The payer is a Foundry keystore
   holding a throwaway test key with some Sepolia ETH for gas (public faucet); no key is ever in
   the environment, on a command line, or in CI. The Sepolia test PHA token
   (`0x8F40e7E99678F44c88158f049E62817580ab113B`) is the repository's `MockERC20`, whose
   `mint(address,uint256)` is public (checked with an `eth_call` from an arbitrary address), so
   the driver mints exactly the locked amount to the payer and then transfers it to the quote
   address. Write `driver.json` with the `SandboxConfig` fields the driver reads:

   ```json
   {
     "service_url": "<TOPUP_ORIGIN>",
     "product_slug": "phala-cloud",
     "product_keyid": "phala-cloud/v1",
     "route": "phala-cloud-sepolia-pha-usd",
     "chain_id": 11155111,
     "rpc_url": "<a Sepolia RPC>",
     "factory": "0x2407bE5Be2b632F5b166872A49E4946a70CCa531",
     "implementation": "0x70B714508BFa441449DC09f790Ca03Baa5170360",
     "token": "0x8F40e7E99678F44c88158f049E62817580ab113B",
     "token_symbol": "PHA",
     "public_url": "<product URL>"
   }
   ```

   The flusher sweeps only forwarders holding at least the route's `min_flush_atomic`, 20000
   test PHA, so pay at least that in one deposit: `--min-atomic` refuses to pay a smaller quote
   and prints the `--amount-minor` needed (the unpaid lock simply expires). `--amount-minor` is
   cents; for 20000 PHA at a PHA price of `$p` it is about `2000000 * p` plus 1-2% margin. The
   quote must also fit the product's per-deposit cap and the route's open-lock cap per account
   (both 500000 cents), so this works while PHA is below about $0.24.

   ```sh
   export ETH_KEYSTORE=~/.foundry/keystores/staging-payer   # cast wallet import staging-payer --interactive
   export ETH_PASSWORD=~/staging/payer.password             # file holding the keystore password
   uv run --locked --project sdk/python python sdk/examples/phala_cloud_integration.py deposit \
     --config driver.json --driver-seed-file ~/staging/driver.seed \
     --amount-minor <cents> --min-atomic 20000000000000000000000
   ```

   The driver registers a fresh workspace through the product, gets a quote-first lock and
   recomputes its address from the product slug, workspace, and `lock_ref` before paying,
   pays the exact `amount_atomic`, and polls the product until the deposit is `credited`, the
   verified `deposit.credited` webhook is recorded, and the product ledger holds exactly one
   credit of the locked amount. Sepolia finality takes about 15 minutes.
8. **Observe the sweep:** the route's flush schedule is `0 */6 * * *` (UTC); a forwarder at or
   above `min_flush_atomic` is swept into the treasury once the flusher's operator holds
   `OPERATOR_ROLE` and gas ([Flusher operator](#flusher-operator)) and the gas ratio allows it.
   To wait for it in the same run, give step 7 `--until swept --timeout 25200` (up to seven
   hours); otherwise check the treasury's token balance with `cast call` after the next
   scheduled run.

`make cvm-rehearsal` runs this product CVM locally: the rendered product compose, the unsealed
env, a re-rendered public URL (which must recreate the container), the sealed env, and one deposit
driven by `deposit` mode.

### Abnormal paths

The deposit driver also plays the sandbox scenarios' abnormal payments
([sandbox/README.md](sandbox/README.md#scenarios)) against staging, through the product CVM, and
checks the outcome from the product's view: the deposit state, the verified webhooks, and the
product ledger. The scenarios themselves (`scenarios/run.py`) cannot run here: they serve their
own product endpoint and need the product key, while staging settles with the product CVM. Each
run registers a fresh workspace, so runs are independent. Every step is **HUMAN-ONLY**, with
`driver.json`, `ETH_KEYSTORE`, and `ETH_PASSWORD` exactly as in step 7 of
[End-to-end order](#end-to-end-order).

| Path | Options | Pays | Expected final state (docs/architecture.md §7, §9, §15) |
|---|---|---|---|
| (a) underpayment | `--pay-bps 9700` | 97% of a fresh quote, outside the 1% `lock_tolerance_bps` | `credited`, then `swept`: valued at spot for what arrived (`deposit.confirmed` `price_source` `spot`), about 3% below the quoted credit; the lock is not consumed and later expires (`rate_lock.expired`); one product credit of the deposit's credit |
| (b) after the quote window | `--pay-after-expiry` | the quoted amount, 60 s after `expires_at` | `rate_lock.expired`, then `credited` at spot and `swept`; the deposit keeps its `lock_ref` and the lock stays `expired` |
| (c) persistent address | `--persistent ATOMIC` | `ATOMIC` to the workspace's persistent address, no quote | `credited` at spot, then `swept` |
| (e) unsupported token | `--persistent ATOMIC --token T --until rejected` | an unrouted token `T` | only after finality (the head scan ignores unrouted tokens): `rejected`, `deposit.rejected` reason `unsupported_asset`; the product is never asked to settle; the flusher sweeps only the route asset, so the tokens stay in the forwarder; `TopupUnsupportedInflows` fires where metrics are scraped |
| (d) refund | `--persistent ATOMIC --until refunded --refund-to A` | more than the route's `max_deposit_atomic` (200000 test PHA) | `rejected` reason `out_of_bounds` (refundable, §15), swept to the treasury with other funds; the driver files a refund request for the whole deposit (`requested`) and waits while finance runs [refund-execution.md](runbooks/refund-execution.md): `approved`, `sent`, then `confirmed` and one `deposit.refunded` webhook |

Credited runs pay at least the route's `min_flush_atomic` (20000 test PHA) so the flusher sweeps
them; a smaller credited deposit is never swept and raises `TopupDepositStateAgeExceeded` after
48 hours. `--min-atomic` applies to the amount actually paid. As in step 7, the credited rows
need PHA below about $0.24, or the product refuses them with `per_deposit_cap`. A refund is
exercised on an out-of-bounds deposit because its funds reach the treasury by the regular flush
and the Safe refunds them with one token transfer; refunding an unsupported token first needs a
separately reviewed Safe flush of that token (refund-execution.md, decision tree), which this
plan does not run.

The refund request and the `rate_lock.expired` events in the account view need a product CVM
running this driver's release: run Release images and Deploy staging product in mode `upgrade`
with the new `product_image` first. The other rows also work with the earlier product image.
Each run costs the payer two Sepolia transactions (`mint`, `transfer`, about 120000 gas; under
0.001 ETH at 5 gwei) and mints its test tokens for free; the sweeps cost the flusher operator its
usual flush gas, and the refund costs the Safe one ERC-20 transfer.

```sh
export ETH_KEYSTORE=~/.foundry/keystores/staging-payer
export ETH_PASSWORD=~/staging/payer.password
rpc=$(jq -er .rpc_url driver.json)
driver=(uv run --locked --project sdk/python python sdk/examples/phala_cloud_integration.py
  deposit --config driver.json --driver-seed-file ~/staging/driver.seed --timeout 3600)
# (a) underpayment: prints the --amount-minor needed if 97% of the quote is below 20000 PHA
"${driver[@]}" --amount-minor <cents> --min-atomic 20000000000000000000000 --pay-bps 9700
# (b) payment after the quote window: about 35 minutes
"${driver[@]}" --amount-minor <cents> --min-atomic 20000000000000000000000 --pay-after-expiry
# (c) persistent address, no quote
"${driver[@]}" --persistent 20000000000000000000000
# (e) unsupported token: the Sepolia unsupported test token, expected to be a MockERC20
unsupported=0x287E3577c66866a3F5Cb7a8Dac6761EB43608392
cast call "$unsupported" 'decimals()(uint8)' --rpc-url "$rpc"
cast call --from 0x000000000000000000000000000000000000dEaD "$unsupported" \
  'mint(address,uint256)' "$(cast wallet address)" 1 --rpc-url "$rpc"
"${driver[@]}" --persistent 1000000000000000000000 --token "$unsupported" --until rejected
# (d) refund: back to the payer, an address the operator controls; waits up to 12 hours
"${driver[@]}" --persistent 200001000000000000000000 --until refunded \
  --refund-to "$(cast wallet address)" --timeout 43200
```

For (e), `decimals` must print 18 and the `mint` call must succeed (an `eth_call`, nothing is
sent); otherwise skip the row. For (d), once the driver logs `REFUND_ID=...` the admin key holder
approves it with the signed `POST /v1/admin/refunds/$REFUND_ID/approve` (require
`status=approved`). The Safe owner waits for the out-of-bounds deposit's sweep (the treasury's
`balanceOf` of the test token grows by the deposit amount), then submits
`transfer(<refund destination>, <amount>)` on the test token from the treasury Safe, and the
admin key holder records the transaction hash with `POST /v1/admin/refunds/$REFUND_ID/record`,
all exactly as [refund-execution.md](runbooks/refund-execution.md) shows. After the transfer is
final the driver logs `refund ... is confirmed` and exits 0. Any driver failure exits non-zero
with the reason; a rejection where a credit was expected, or the reverse, is a failure.

## Local verification

The local stack is the attested `deploy/docker-compose.yml`, rendered with local settings by
[local/compose.sh](local/compose.sh), plus the `deploy/local/docker-compose.yml` overlay, which adds
Garage (S3), the dstack simulator, and a mock product, builds the images from the checkout, and
replaces secrets, ports, and host paths with local values. It builds dstack's simulator from the
pinned source revision and shares its `/var/run/dstack.sock` with `topup` and `keys`, so local
stacks derive their database credentials exactly as a CVM does. Run manual commands through the
wrapper:

```sh
deploy/local/compose.sh ps
```

```sh
make up
make down
make infra-smoke
SERVICE_SMOKE=1 deploy/local/service-smoke.sh
```

A local stack created before the PostgreSQL 18 upgrade keeps a PostgreSQL 16 volume mounted at the
old path, which the new image does not read. Remove it (local data only) before `make up`:

```sh
deploy/local/compose.sh down -v
```

`infra-smoke.sh` tests migrations, route-template validation, simulator attestation, the running
backup service, WAL-G dry-run commands, and PostgreSQL archive settings, then removes its containers
and volumes. `service-smoke.sh` is opt-in and requires `topup run --help` to expose the unified
`--bind` and `--route` options. It starts the service with the application database role, local
administrative verification key, mounted route, and scanner provider URL; then it checks the TCP
listener, `GET /healthz` for HTTP 200, `GET /openapi.json`, and the running backup service. The local
provider URL is deliberately unreachable, exercising scanner retry behavior without contacting a
real chain. A skipped service smoke is not a successful service check.

Backup encryption, Garage object storage, point-in-time recovery, and the weekly destructive drill
are documented in [RESTORE.md](RESTORE.md). Run `make restore-drill`; it uses an isolated Compose
project and removes all drill containers and volumes on exit.

### CVM rehearsal

`make cvm-rehearsal` ([local/cvm-rehearsal.sh](local/cvm-rehearsal.sh)) runs the staging
artifact itself rather than the local overlay: it pushes both images to a throwaway loopback
registry, deploys the factory (mock Safe as admin and treasury), test token, and sanctions oracle
to an Anvil chain with Sepolia's chain id using the A2 and sandbox scripts, writes the staging
route with those addresses into a copy of the compose, renders it with `render-compose.sh` and
the provisional origin, and starts it with the unsealed `.env` (exactly the `staging.env.example`
names, empty): PostgreSQL must refuse to initialize without a listable backup prefix, and the
re-rendered gateway origin must recreate every service. Then it seals the secrets.
[local/cvm-rehearsal.compose.yml](local/cvm-rehearsal.compose.yml) adds only the dstack simulator
(in place of the host socket), Garage (S3), and Anvil. The run asserts that `migrate` exits 0, `topup`
passes its startup contract check and serves `/healthz`, `/v1/attestation` binds a fresh nonce
and the flusher operator through the simulator (the same values as `topup attest --route`), the
flusher waits for that attested address's `OPERATOR_ROLE` and resumes once the mock Safe grants it
and it is funded, the backup marker is fresh, the product is issued through the signed
`POST /v1/admin/products`, and one quote-first deposit is credited end to end against the
reference product with a lock priced from the live HTTPS sources (so the image's
TLS verification with system roots works), then prints the workload's memory and checks that no
container, volume, network, or image of the run is left. It needs Foundry with `contracts/lib`, the
Docker host's loopback (for the registry and Anvil), and internet access for the live price
sources; it bind-mounts nothing.

## Pinned upstream references

- dstack boundaries and encrypted env:
  <https://github.com/Dstack-TEE/dstack/blob/282eeb27d22d8f091ad0fa5a90e638f85cf68751/docs/security/cvm-boundaries.md>
- dstack socket, gateway, and simulator usage:
  <https://github.com/Dstack-TEE/dstack/blob/282eeb27d22d8f091ad0fa5a90e638f85cf68751/docs/usage.md>
- dstack verification:
  <https://github.com/Dstack-TEE/dstack/blob/282eeb27d22d8f091ad0fa5a90e638f85cf68751/docs/verification.md>
- dstack verifier (request, result, and checks):
  <https://github.com/Dstack-TEE/dstack/blob/282eeb27d22d8f091ad0fa5a90e638f85cf68751/verifier/README.md>
- Phala CLI 1.1.22 deploy implementation:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts>
- Phala CLI 1.1.22 flags:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/command.ts>
- dstack `DstackApp` authorization contract:
  <https://github.com/Dstack-TEE/dstack/blob/282eeb27d22d8f091ad0fa5a90e638f85cf68751/kms/auth-eth/contracts/DstackApp.sol>
