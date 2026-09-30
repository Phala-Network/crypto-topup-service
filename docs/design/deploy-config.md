# Design: a lean, standard deployment configuration

Status: proposed, in review (Phase 1 of the refactor; nothing is implemented yet). The owner's
decisions of 2026-09-30 are applied: staging's values are the current `staging` Environment values
(they match the last attested compose); the Sentry release is the compiled source commit;
`DATABASE_URL` is the only database variable, and `migrate` and `restore-check` check in code that
the login owns the database; and the renderer uses a Compose binary pinned by sha256.
Scope: the attested compose, its variants, every setting of `topup` and of the deployment, and the
scripts, workflows, and docs around them. Owner's rules: "精简优雅重构compose和各种配置项"; use a
standard mechanism wherever one exists; don't abuse env.

Claims about third-party behaviour were checked on 2026-09-30 against source or by experiment.
The **dstack-0.5.9 guest runs Docker Compose v2.26.0**: its `meta-dstack` v0.5.9 pins
`meta-virtualization` 52cd8a2, whose `docker-compose_git.bb` has `PV = "v2.26.0"`. The guest starts
the stack with `docker compose up --remove-orphans -d --build` in `/dstack`, with the sealed env as
its process environment (dstack v0.5.9 `basefiles/app-compose.{sh,service}`:
`EnvironmentFile=-/dstack/.host-shared/.decrypted-env`).

## 1. Decisions

1. **One typed config file for topup** per environment (`topup.yaml`: origin, admin key, RPC
   provider URLs, routes). It is committed, reviewed by PR, and inlined into the attested compose
   as a Compose `config`. topup reads it with `--config`; every `TOPUP_*` setting variable is
   removed.
2. **Env only where env is the interface**: the sealed secrets (unchanged, still `${NAME}`
   references in the compose), libpq's `DATABASE_URL`/`PGPASSFILE`, and the env interfaces of
   third-party images (WAL-G's `WALG_*`/`AWS_*`, dstack-ingress's `DOMAIN`). Those are written as
   literals in the environment's compose overlay, never taken from the environment at render
   time.
3. **Variants are standard Compose overrides.** `compose.yaml` is the service stack; the
   restore-check variant is `compose.restore-check.yaml`, which removes services with `!reset
   null` and adds its own. `docker compose config` merges them. The `# only-in:` template language
   and `render-compose.sh` are deleted.
4. **Public settings move out of GitHub.** Environment variables go from 13 (plus 3 optional
   overrides and 4 derived values) to 3 deployment-state variables. Deploy derives nothing.
5. **Rendering uses Compose itself**, pinned to v2.26.0, the version the CVM runs:
   `docker compose config --no-interpolate`, then three named deploy-time inputs (image digests,
   the gateway domain, the restore instance's origin). The output is Compose's canonical YAML.
6. **Services:** `heartbeat`, `keys`, and `migrate` stay (§6). The `rendered-sha256` label is
   replaced by config names that carry their content digest. `restore` moves to the local drill
   overlay.
7. **Self-hosting without a fork is a follow-up PR** (§10). This PR makes an environment one
   directory, which is what that follow-up needs.

## 2. Today's surface

| Item | Count |
|---|---|
| `deploy/docker-compose.yml` | 561 lines, 106 of them comments; 10 services; 2 `x-` anchors; 5 inline configs (4 route copies, 128 non-blank lines duplicated from `deploy/config/routes/`, and the Postgres init script, duplicated from `postgres-init/`) |
| `${NAME}` placeholders in the compose | 23 names: 5 sealed secrets, 2 images, 2 mode switches (set by the variant), 1 label digest, 13 public settings |
| Sources of one public setting | a GitHub Environment variable, a derivation in `deploy.yml`, the awk/bash renderer, and the compose placeholder |
| GitHub Environment variables (`staging`) | 13 documented, plus 3 optional overrides (`AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`, `TOPUP_ADMIN_KID`) and 4 derived settings |
| Lists of sealed names | 3 kept in sync: `staging.env.example`, `app-compose.example.json` `allowed_envs`, the compose's `${…}` references |
| Environment variables `topup` reads | 15 fixed names plus 2 per RPC provider; 4 of them only ever carry a code default or a constant (`TOPUP_BACKUP_TIMESTAMP_FILE` ×3, `TOPUP_WEBHOOK_PROXY`) |
| Render and check scripts | `render-compose.sh` 171, `validate-compose.sh` 287, `preflight.sh` 487 (awk/sed YAML parsing of routes), `check-route-modes.sh` 70, `render-app-compose.sh` 37, `write-staging-env.sh` 28, `product/render-compose.sh` 14, `local/compose.sh` 36, `sandbox/render-sepolia-compose.sh` 31 |
| `deploy.yml` | 524 lines: 11 settings in the job `env`, 1 step that copies `TOPUP_RPC_*_URL` from `vars`, 1 step that derives R2 and key-id defaults |
| Adding a chain | 6 places: the route file, its compose copy with its `--route` args in two services, `x-rpc-providers`, GitHub variables, and, for a keyed provider, `staging.env.example` and `allowed_envs` |

Settings that live in GitHub variables can change without a pull request, a review, or a
history, although they fix the money-relevant RPC endpoints, the backup prefix, and the admin key.
Today's values are consistent: the `staging` Environment's 13 variables match the compose attested
by the last topup deploy (run 36670873413). The problem is the model, not a current mistake.

## 3. Principle: four kinds of input, each with one home

| Kind | Home | Attested | Examples |
|---|---|---|---|
| Topology: which services exist, what they mount, who talks to whom | `deploy/compose.yaml` and `deploy/compose.restore-check.yaml` | yes | uid/gid, tmpfs keys, the dstack socket mounts, the smokescreen policy, `--webhook-proxy`, ports |
| Environment settings: public values that differ per deployment | `deploy/environments/<owner>/<env>/` (`compose.yaml` overlay and `topup.yaml`) | yes | origin, admin key, RPC URLs, routes, WAL-G location, ingress domain |
| Deploy-time facts: known only from a release or a CVM | `render.sh` flags | yes | image digests, gateway domain, a restore instance's origin |
| Secrets | the CVM's sealed env, names in `deploy/secrets.env.example` | names only | S3 credentials, `SENTRY_DSN`, `TOPUP_RPC_<ID>_KEY` |

The environment directory is namespaced by repository owner
(`deploy/environments/phala-network/staging`) because Deploy resolves it from
`${GITHUB_REPOSITORY_OWNER,,}`. A fork therefore finds no directory and fails closed. Without the
namespace, a fork that forgot to edit the files would deploy Phala's admin public key and domain
into its own CVM.

## 4. Target surface

### Files

```text
deploy/
  compose.yaml                     service stack (topology only; short pointers into docs)
  compose.restore-check.yaml       variant: !reset null dstack-ingress, smokescreen, heartbeat,
                                   backup; topup --read-only on 8081; + restore-check
  secrets.env.example              the sealed names (was staging.env.example; same in every env)
  render.sh                        pinned compose config + deploy-time inputs (~60 lines)
  product/compose.yaml             reference product stack
  product/secrets.env.example      PRODUCT_API_KEY
  environments/phala-network/staging/
    topup/compose.yaml             overlay: ingress DOMAIN, WAL-G location (~20 lines)
    topup/topup.yaml               typed topup config, the four routes inline with their comments
    product/compose.yaml           overlay: ingress DOMAIN
    product/config.json            the product's config, every value literal
  local/topup.yaml                 local stacks' config (Anvil / 127.0.0.1 providers, local admin key)
```

Deleted: `docker-compose.yml`, `render-compose.sh`, `render-app-compose.sh`,
`app-compose.example.json`, `staging.env.example`, `write-staging-env.sh` (now a one-line `sed` in
Deploy), `check-route-modes.sh` (moved into preflight, below), `config/routes/*.yaml` (moved into
`topup.yaml`), `product/render-compose.sh`, `local/compose.sh` (local stacks merge the source files
directly), and `sandbox/render-sepolia-compose.sh` (the sandbox becomes an environment directory).

### `topup.yaml`

```yaml
environment: staging                      # Sentry environment (`-restore` suffix when --read-only)
public_origin: https://pay-api-staging.phala.com
admin_key:
  id: admin/staging-v1
  public_key: 23Y9wEJMOTySGV3UXmcTFnQsbigA9/cYTvmqdQxzmdo=
rpc_providers:                            # id → URL; `{key}` takes the sealed TOPUP_RPC_<ID>_KEY
  provider-a: https://sepolia.gateway.tenderly.co
  provider-b: https://ethereum-sepolia-rpc.publicnode.com
  base-sepolia-a: https://base-sepolia.gateway.tenderly.co
  base-sepolia-b: https://base-sepolia-rpc.publicnode.com
routes:                                   # each item is today's route file, unchanged
  - route: phala-cloud-sepolia-pha-usd
    version: 3
    ...
```

The file is parsed with serde and `deny_unknown_fields`, as routes are, and `topup config check`
validates it. The route schema is unchanged, so routes still name provider ids in
`chain.rpc_providers`. Preflight's bash checks move into Rust: every provider a route names exists
and none is unused; each provider serves one chain; each route's providers have distinct URLs; and
each `{key}` URL has its key. New commands: `topup config check FILE` and `topup config show FILE`
(resolved JSON). `topup route validate|show` stay for single route files (`examples/`, the sandbox
template).

### Compose (sketch of the parts that change)

```yaml
# compose.yaml, the service stack
services:
  topup:
    image: phala-pay                      # render.sh pins it to the release digest
    command: [topup, run, --config, /etc/topup/topup.yaml, --webhook-proxy, http://smokescreen:4750]
    environment:
      DATABASE_URL: postgres://topup_service@postgres:5432/topup
      PGPASSFILE: /run/db-app/topup_service.pgpass
      SENTRY_DSN: ${SENTRY_DSN:-}
      TOPUP_RPC_PROVIDER_A_KEY: ${TOPUP_RPC_PROVIDER_A_KEY:-}
      TOPUP_RPC_PROVIDER_B_KEY: ${TOPUP_RPC_PROVIDER_B_KEY:-}
    configs: [{source: topup, target: /etc/topup/topup.yaml}]
    ...
# compose.restore-check.yaml
services:
  dstack-ingress: !reset null
  smokescreen: !reset null
  heartbeat: !reset null
  backup: !reset null
  postgres: {environment: {TOPUP_RESTORE_FROM_BACKUP: "on"}}
  topup:
    command: [topup, run, --config, /etc/topup/topup.yaml, --read-only,
              --restore-report, /run/topup-observability/restore-check.json]
    ports: ["8081:8080"]
  restore-check: {...}                    # exists only in this variant
```

### Rendering

```sh
deploy/render.sh --images images.json --gateway-domain gateway.dstack-pha-prod5.phala.network \
  topup deploy/environments/phala-network/staging/topup >docker-compose.staging.yml
deploy/render.sh --restore-check --images images.json --origin https://<app_id>-8081.<gateway> \
  topup deploy/environments/phala-network/staging/topup >restore-check.yml
```

1. `docker compose -p dstack --project-directory deploy -f compose.yaml -f ENV/compose.yaml
   [-f compose.restore-check.yaml] config --no-interpolate --format json`. This merges the
   overlays, resolves anchors, and keeps the secret `${…}` references and `$$` escapes.
2. `jq` applies exactly three named inputs. It pins every `phala-pay`, `postgres-walg`, and
   `phala-pay-reference-product` image to the release's `repository@sha256` and fails if an image
   has no digest. It sets dstack-ingress's `GATEWAY_DOMAIN` (service), or appends
   `--public-origin URL` to the read-only topup (restore-check). It inlines each `file:` config as
   `content` with `$` escaped, named `<name>_<sha256[:12]>`.
3. `docker compose -f - config --no-interpolate` turns the result into canonical YAML.

`images.json` from Release images is keyed by image name (`phala-pay`, `postgres-walg`,
`phala-pay-reference-product`). This PR changes the binary, so the first deploy after it uses a new
release anyway and needs no compatibility with the old keys.

The pinned binary is fetched once by release URL and sha256 (Linux and macOS, amd64 and arm64).
Pinning makes the output reproducible on any machine, which preflight's fresh-render byte
comparison needs. It also proves the artifact loads on the CVM's own Compose.

Verified on Compose v2.26.0 and v5.5.1, on the same sample: `--no-interpolate` keeps
`${S:-}` and `$$`; `!reset null` removes a service; the output of both versions is byte-identical;
and v2.26.0 loads it. The output bakes the project name into resource names (`name: dstack`,
`dstack_pgdata`, `dstack_default`). These are exactly the names the CVM derives today from
`/dstack`, so the existing volumes are kept. The attested file states them explicitly.

## 5. Every setting, before and after

| Setting | Today | After |
|---|---|---|
| `TOPUP_IMAGE`, `POSTGRES_WALG_IMAGE`, `PRODUCT_IMAGE` | `${X_IMAGE:-zero digest}` ×10, rewritten by awk | `image: phala-pay` etc.; `render.sh --images` pins them |
| `TOPUP_IMAGE` in topup's env (Sentry release) | env | removed; release is the source commit, compiled in (`SOURCE_COMMIT` build arg beside `SOURCE_DATE_EPOCH`) |
| dstack-ingress image | pinned digest in compose | unchanged |
| `TOPUP_DOMAIN` | GitHub var, rendered twice | overlay `dstack-ingress.environment.DOMAIN` and `topup.yaml` `public_origin`; `validate-compose.sh` checks they agree |
| `TOPUP_PUBLIC_ORIGIN` | env `https://${TOPUP_DOMAIN}` | `topup.yaml` `public_origin`; restore-check: `render.sh --origin` → `--public-origin` |
| `TOPUP_GATEWAY_DOMAIN` | read from the CVM, rendered | unchanged source, now `render.sh --gateway-domain` |
| `TOPUP_ADMIN_KID` | derived `admin/<env>-v1`, or a variable | `topup.yaml` `admin_key.id`, explicit |
| `TOPUP_ADMIN_PUBLIC_KEY` | GitHub var | `topup.yaml` `admin_key.public_key` |
| `TOPUP_RPC_<ID>_URL` | GitHub vars → `x-rpc-providers` → env of 2 services | `topup.yaml` `rpc_providers` |
| `TOPUP_RPC_<ID>_KEY` | sealed | sealed, unchanged (same names, so no re-seal) |
| Routes | 4 files + 4 compose copies + 8 mounts + 8 `--route` args | `topup.yaml` `routes`; one config, mounted in 2 services |
| `WALG_S3_PREFIX`, `AWS_ENDPOINT` | GitHub vars | overlay literals (WAL-G's env interface) |
| `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE` | derived for R2, or a variable | overlay literals, explicit |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `SENTRY_DSN` | sealed | unchanged |
| `SENTRY_ENVIRONMENT` | derived from the Environment's name | `topup.yaml` `environment` |
| `TOPUP_SERVICE_ENABLED` (`on`/`read-only`/`off`) | set by the renderer in 2 services | `topup run --read-only` in the variant; `heartbeat` does not exist there; `off` is removed (only a test used it) |
| `TOPUP_RESTORE_FROM_BACKUP` | set by the renderer in 3 services | only `postgres`, `"on"`, in the variant (the entrypoint defaults to `off`); the gates in `backup` and `restore-check` go, since each service exists only in its own variant |
| `TOPUP_WEBHOOK_PROXY` | env | `--webhook-proxy`, beside the smokescreen service it names |
| `TOPUP_RESTORE_REPORT_FILE` | env in 2 services | `--restore-report` (run) and `--report` (restore-check) |
| `TOPUP_BACKUP_TIMESTAMP_FILE` | env in 3 services, always the default | removed |
| `MIGRATE_DATABASE_URL` / `DATABASE_URL` | 2 names | `DATABASE_URL` in each service; `migrate` and `restore-check` refuse unless the login owns the database, a stronger guard than a variable name |
| `PGPASSFILE` | env | unchanged (libpq) |
| `heartbeat --interval-s 60` | flag | removed (the default) |
| `phala-pay.rendered-sha256` label, `${…_RENDERED_SHA256}` | on every service | removed; config names carry their digest (§6) |
| `allowed_envs` | `app-compose.example.json` | `secrets.env.example`; checked against the compose's `${…}` names |
| `PHALA_WORKSPACE`, `TOPUP_CVM_ID`, `STAGING_PRODUCT_CVM_ID`, secret `PHALA_CLOUD_API_KEY` | GitHub | unchanged: deployment state and tool credentials, not attested |
| `PRODUCT_DOMAIN`, `PRODUCT_DRIVER_PUBLIC_KEY`, `TOPUP_ORIGIN`, `PRODUCT_PUBLIC_URL` | GitHub vars and derivations | product overlay `DOMAIN` and literal values in `config.json` |
| OS image `dstack-0.5.9`, CLI `phala@1.1.22` | `deploy.yml` | unchanged |

After: `${…}` names in the compose go from 23 to 5 (the secrets), GitHub Environment variables
from 13 (+3 optional) to 3, and the fixed environment variables `topup` reads from 15 to 3
(`DATABASE_URL`, `PGPASSFILE`, `SENTRY_DSN`), plus each `TOPUP_RPC_<ID>_KEY`. The dev-only
`DSTACK_SIMULATOR_ENDPOINT` stays. Adding a chain touches one file (`topup.yaml`), plus
`secrets.env.example` and the compose's key line for a keyed provider.

## 6. Services

| Service | Decision | Evidence |
|---|---|---|
| `keys` | stays | The Postgres and WAL-G images have no dstack client, so something must derive their passwords and backup key into tmpfs. Keeping it separate means `postgres`, `backup`, and `migrate` never mount the dstack socket. Caveat: `topup`, `restore-check`, and dstack-ingress do mount it, and any socket holder can call `get_key` for any path. The boundary is therefore that third-party images never hold the socket, and that topup is given no owner credentials by configuration. It is not a hard wall against a compromised topup. The doc will say so. |
| `migrate` | stays | A one-shot init job gated by `service_completed_successfully` is the Compose idiom. Folding it into topup would hand topup the owner login. |
| `heartbeat` | stays, service variant only | Its row is the RPO anchor (`restored_heartbeat_at` within 120 s of the failure point) and the steady commit that keeps WAL flowing. `topup run` checks the contracts over RPC before it touches the database, waits for the lease-owner lock, and exits on any task failure. Folded into it, an RPC outage or a crash loop would stop the heartbeat and yield false RPO findings on the next restore. It no longer reads a mode variable, since the restore-check variant simply lacks it. |
| `smokescreen`, `backup` | service variant only | Today they idle in restore-check. Removing them there makes "never writes the prefix" structural rather than a switch. |
| `restore` (profile `tools`) | moves to `local/restore-drill.compose.yml` | Only `restore-drill.sh` runs it. It is attested today but never started on a CVM. |
| `rendered-sha256` label | replaced | Compose v2.26.0 hashes only the service definition (`pkg/compose/hash.go`) and copies config content only at container creation (`createMobyContainer` → `injectConfigs`). An upgrade that changes only a config's content would therefore keep the old file. The need remains. Naming each config after its digest is Compose's documented config rotation practice (a new name per content, as with kustomize's hashed ConfigMaps). The service definition changes exactly when its content does, so only the affected services are recreated. A route change no longer restarts PostgreSQL. |

## 7. Alternatives rejected, with evidence

- **`profiles` for variants.** Profile selection happens at runtime (`--profile` or
  `COMPOSE_PROFILES`); dstack runs `docker compose up` without either, and `config` output keeps
  `profiles:`, so an inactive service would not start. A variant chosen at runtime would also not
  be one attested file.
- **`include`.** It imports whole projects and cannot modify an existing service, so it cannot
  express "topup read-only on 8081".
- **Compose `secrets:` with `environment:` sources** (the standard secret mechanism). Compose
  v2.26.0 writes the secret into the container only when it creates the container. After
  `phala envs update` (a restart, with an unchanged service hash) every container would keep the
  old secret. Interpolated `${NAME}` references change the service hash, so they recreate the
  container. Tested; stays as today.
- **`env_file:` with dstack's decrypted env.** It would hand every sealed secret to every service
  that lists it (topup would get the S3 credentials, PostgreSQL the RPC keys).
- **Interpolating public settings at render time.** `config` without `--no-interpolate` resolves
  the secret references too (to empty strings), and escaping them as `$${…}` survives the render as
  literals. Verified. Deploy-time values therefore go through three explicit jq edits instead.
- **Keeping public settings in GitHub variables.** They are mutable without review or history
  (§2), and the renderer, the derivations, and the copy step exist only to carry
  them.
- **Routes as separate files next to `topup.yaml`.** That keeps 4 compose configs and mounts, and
  makes the renderer discover files. Inline routes keep one file, one config, and their comments.

## 8. Scripts and workflows after the change

- `validate-compose.sh` keeps its policies and runs them on the two rendered variants: credential
  mounts, egress, single ingress, no password in env, the secret names equal to
  `secrets.env.example`, and the variant differing from the service only as §4 says. It drops the
  route-copy comparisons, the name-mangling jq, and `app-compose.example.json`.
- `preflight.sh` drops the awk/sed YAML parsing. Locally, it checks the env file against
  `secrets.env.example`, that images are pinned, and a byte-equal fresh render, then runs
  `topup config check` in the pinned image. That is the only step with network access in
  `--offline` mode: an anonymous pull, and no RPC or Phala Cloud call. Online, it adds the RPC and
  asset checks from `topup config show` JSON with jq, and the route-mode policy: a known chain
  list, no devnet, and no live route when `environment: staging`. This replaces
  `check-route-modes.sh`.
- `deploy.yml` loses the settings `env` block, the RPC copy step, and the derivations. Its render
  step becomes the `render.sh` call above.
- Release images writes the new `images.json` keys and runs `render.sh`'s image check.
- The local stacks (`make up`, the sandbox, the restore drill) run
  `docker compose -f deploy/compose.yaml [-f compose.restore-check.yaml] -f local/…` on the
  sources under their own project name, with `local/topup.yaml`. The CVM rehearsal runs
  `render.sh --project-name` output, so concurrent rehearsals never share the `dstack_*` volumes.
- Tests: `deploy/tests/render-compose.sh` becomes a `render.sh` test. `cargo test` runs
  `topup config check` on every committed environment and on `local/topup.yaml`. The tests of the
  deleted scripts go with them.
- Docs: `deploy/README.md` ("One-time setup" step 4, "Attested settings", "RPC providers",
  "Sealing the secrets", "Database credentials"), `RESTORE.md` (render commands), `phala.md`,
  `docs/self-hosting.md` §2 and §3, `docs/configuration.md` (rewritten around `--config`),
  architecture §14 references, and the `TOPUP_PUBLIC_ORIGIN` wording in the admin OpenAPI (so
  `openapi.admin.json` regenerates). The compose keeps only one-line pointers into these docs, and
  the canonical render drops compose comments anyway. The route comments survive inside the
  config content. The CHANGELOG records only integrator-visible changes, so this refactor adds no
  entry there unless the owner wants an "Operators" section.

## 9. Migration

**Phala's staging** (one Deploy `upgrade`; no re-seal, no data movement):

1. The values for `deploy/environments/phala-network/staging/` are the current `staging`
   Environment values (owner decision). They match the compose attested by run 36670873413; the
   §11 diff shows any transcription error.
2. Merge, run Release images (the binary changed), and Deploy `upgrade` with that release.
   - Kept: the sealed names, the volume names (`dstack_*`), the app id, and the domain.
   - The compose hash changes, as with any upgrade.
   - Every container is recreated once, because each definition changed.
   - PostgreSQL restarts once on its unchanged volume.
3. Afterwards, delete the now unused `staging` variables: `TOPUP_DOMAIN`, `AWS_ENDPOINT`,
   `TOPUP_ADMIN_PUBLIC_KEY`, the `TOPUP_RPC_*_URL`, `PRODUCT_DOMAIN`, and
   `PRODUCT_DRIVER_PUBLIC_KEY`. Deploy stops reading them in this PR, so leaving them is harmless.
4. The product CVM: `upgrade` with target `product`, the same way.
5. A restore from a backup taken before the upgrade works unchanged: the prefix, key, and passwords
   are derived exactly as before. The restore-check variant is rendered from the commit being
   restored to, as `RESTORE.md` already requires.

**Self-hosters (forks).** Create `deploy/environments/<owner>/<env>/topup/` from Phala's staging
directory, with their own values taken from their GitHub variables. Then run Release images and
Deploy `upgrade`. Their sealed env is untouched. Deploy fails closed until the directory exists.

**Phala Cloud template** (Phala-Network/phala-cloud#520, open). It mirrors today's compose with
`${VAR:-default}` public settings. It needs a follow-up there: the rendered service compose, with
`topup.yaml` as an inline config whose values are the template's `${…}` form fields. Compose
interpolates config `content`, so topup needs no change for that.

## 10. Self-hosting without a fork: a follow-up

It is possible, but not in this PR. It needs three things this change does not add:

- Versioned releases: tags, notes, and images published per version rather than per dispatch.
- A distributable deploy kit consumed at a tag: `compose*.yaml`, `render.sh`, and a reusable
  `workflow_call` Deploy, or a small CLI.
- New trust documentation: operators verify Phala's reproducible images instead of building their
  own.

This PR makes the follow-up small. An instance becomes one environment directory, which
`render.sh` accepts from any path, plus a release's `images.json`. Mixing both changes into one
review would put the live staging migration and a new distribution model at risk together.

## 11. Verification (Phase 2)

- **Behavioural equivalence.** Render the old compose with run 36670873413's inputs and the new
  one with the same release and gateway. Normalize the old one with
  `docker compose -p dstack config --no-interpolate`, diff the two, and explain every difference:
  - expected: moved settings, removed defaults and label, renamed configs, removed idle services,
    image fields;
  - anything else is a bug.
- **Scripts:** `validate-compose.sh`, `preflight.sh --offline`, the `deployment` CI job's tests,
  `cargo test`, and `make lint`.
- **Local stacks:** `make restore-drill` (both modes) and `make cvm-rehearsal`.
- **Limits:** `CARGO_BUILD_JOBS=8`; every container, volume, and temp file the runs create is
  removed.

## 12. Risks

- **The artifact's form changes** (canonical YAML, no comments, explicit resource names). A
  script that reads the attested compose by today's keys breaks: in this repo,
  `verify-attestation.sh` and the runbooks, which are updated here. Merchants pin only the compose
  hash (`docs/integration.md` §5.3), which changes on every upgrade anyway.
- **A new tool dependency**: the Compose binary pinned by sha256. A renderer on another version
  could produce different bytes. `render.sh` refuses any other version, rather than silently using
  the system Compose.
- **Volume identity.** Rendering under any project name but `dstack` would move the CVM onto new
  empty volumes, and PostgreSQL would then restore from backup or wait. `render.sh` fixes
  `-p dstack` for CVM output, and `validate-compose.sh` asserts `dstack_pgdata`.
- **Moving validation into Rust** changes where errors surface (`topup config check` rather than
  bash). The rules are carried over one for one, with tests.
- **Transcription**: the committed staging values must equal the Environment's, or the upgrade
  would change the backup prefix or the RPC endpoints. The §11 diff against the last attested
  compose shows any difference before the upgrade.

## 13. What stays, and why

- Attestation of every public setting that affects money or trust. Each one is in `topup.yaml`,
  an overlay, or a pinned image, all inside the one attested file.
- The sealed secrets, their names, and `allowed_envs`. The admin seed never enters the CVM.
- `keys` and the three tmpfs volumes with their mount policy, and the pinned dstack-ingress
  (tls-alpn-01, 443 only).
- The smokescreen policy flags; WAL-G and its encryption key path; `postgres-init`.
- Deploy's two modes, its read-back and verification, and preflight online and offline.
- Every guarantee of `RESTORE.md`: the restore-check variant publishes only 8081, has no
  `backup`, `heartbeat`, or ingress, runs PostgreSQL with `TOPUP_RESTORE_FROM_BACKUP=on`, and
  never takes live traffic.
