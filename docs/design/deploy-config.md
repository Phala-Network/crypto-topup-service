# Design: a lean, standard deployment configuration

Status: accepted with the review's changes (owner decisions and Astra's review of 2026-09-30 are
applied). Scope: the attested compose, its variants, every setting of `topup` and of the
deployment, and the scripts, workflows, and docs around them. Owner's rules:
"精简优雅重构compose和各种配置项"; use a standard mechanism wherever one exists; don't abuse env.

Claims about third-party behaviour were checked on 2026-09-30 against source or by experiment.
The **dstack-0.5.9 guest runs Docker Compose v2.26.0**: `meta-dstack` v0.5.9 pins
`meta-virtualization` 52cd8a2, whose `docker-compose_git.bb` has `PV = "v2.26.0"`. The guest runs
`docker compose up --remove-orphans -d --build` in `/dstack`, with the sealed env as its process
environment (dstack v0.5.9 `basefiles/app-compose.{sh,service}`:
`EnvironmentFile=-/dstack/.host-shared/.decrypted-env`); `ExecStop` stops the whole stack.

## 1. Decisions

1. **One typed config file for topup** per environment: `topup.yaml`, holding the origin, the
   admin key, the RPC provider URLs, and the routes. It is committed, reviewed by PR, and inlined
   into the attested compose as a Compose `config`. topup reads it with `--config`, and every
   `TOPUP_*` setting variable is removed. `topup config check|show` validate and print it with
   no secret present: a `{key}` stays literal. The service and a preflight that holds the secrets
   resolve the keys through the same code path.
2. **Env only where env is the interface.** That covers the sealed secrets (still `${NAME}`
   references), libpq's `DATABASE_URL`/`PGPASSFILE`, and the env interfaces of third-party images:
   WAL-G's `WALG_*`/`AWS_*` and dstack-ingress's `DOMAIN`. These are written as literals in the
   environment's compose overlay, never taken from the environment at render time.
3. **Variants are standard Compose overrides.**
   - `compose.yaml` holds every service of the topup CVM.
   - Each variant is an override that removes what it does not run with `!reset null`:
     `compose.service.yaml` removes `restore-check`, and `compose.restore-check.yaml` removes
     `dstack-ingress`, `smokescreen`, `heartbeat`, and `backup`.
   - (An inert `profiles:` entry would not do: Compose v2.26 keeps inactive-profile services in
     `config` output, still carrying `profiles:`.)
   - The `# only-in:` template language and `render-compose.sh` are deleted.
4. **Public settings move out of GitHub into reviewed files.** Environment variables go from 13
   (plus 3 optional overrides and 4 derived values) to 3 deployment-state variables.
5. **Rendering uses Compose itself**, pinned by sha256 to v2.26.0, the version the CVM runs:
   `config --no-interpolate`, plus three bounded deploy-time inputs. Those inputs are image
   digests, the gateway domain (service only), and the restore instance's origin (restore-check
   only). Each has a restricted format and there is no general `--set`. The output is Compose's
   canonical YAML.
6. **One artifact policy** (`deploy/compose-policy.jq`) judges the *merged* artifact wherever it
   is checked: `validate-compose.sh`, `preflight.sh`, and `verify-attestation.sh`. It rules on
   the service set, the ports, credential mounts, the egress, restore isolation, and where each
   secret reference may appear.
7. **Services:** `heartbeat`, `keys`, and `migrate` stay (§6). The `rendered-sha256` label is
   replaced by config names that carry their content digest. `restore` moves to the local drill
   overlay.
8. **Fork-independent inputs now, fork-free packaging later** (§10).
   - `render.sh` and preflight accept any environment directory. The `<owner>/<env>` path is only
     Deploy's guard.
   - A generic example environment ships.
   - The environment overlay declares its keyed providers' secrets.
   - Release packaging and the Cloud template follow in later PRs.
9. **Two P1 fixes are in scope.**
   - A `{key}` may only fill a whole path segment or a whole query value, and substitution must
     leave the URL's authority unchanged (§5).
   - The zero-data-loss claim is proved by an in-place upgrade rehearsal on real committed data,
     not by naming (§11).

## 2. Today's surface

| Item | Count |
|---|---|
| `deploy/docker-compose.yml` | 561 lines, 106 of them comments; 10 services; 2 `x-` anchors; 5 inline configs (4 route copies, 128 non-blank lines duplicated from `deploy/config/routes/`, and the Postgres init script, duplicated from `postgres-init/`) |
| `${NAME}` placeholders in the compose | 23 names: 5 sealed secrets, 2 images, 2 mode switches (set by the variant), 1 label digest, 13 public settings |
| Sources of one public setting | a GitHub Environment variable, a derivation in `deploy.yml`, the awk/bash renderer, and the compose placeholder |
| GitHub Environment variables (`staging`) | 13, plus 3 optional overrides (`AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`, `TOPUP_ADMIN_KID`) and 4 derived settings; `verify-contracts.yml` reads 2 of them too |
| Lists of sealed names | 3 kept in sync: `staging.env.example`, `app-compose.example.json` `allowed_envs`, the compose's `${…}` references |
| Environment variables `topup` reads | 15 fixed names plus 2 per RPC provider; 4 of them only ever carry a code default or a constant (`TOPUP_BACKUP_TIMESTAMP_FILE` ×3, `TOPUP_WEBHOOK_PROXY`) |
| Render and check scripts | `render-compose.sh` 171, `validate-compose.sh` 287, `preflight.sh` 487 (awk/sed YAML parsing of routes), `check-route-modes.sh` 70, `render-app-compose.sh` 37, `write-staging-env.sh` 28, `product/render-compose.sh` 14, `local/compose.sh` 36, `sandbox/render-sepolia-compose.sh` 31 |
| `deploy.yml` | 524 lines: 11 settings in the job `env`, 1 step that copies `TOPUP_RPC_*_URL` from `vars`, 1 step that derives R2 and key-id defaults |
| Adding a chain | 6 places: the route file, its compose copy with its `--route` args in two services, `x-rpc-providers`, GitHub variables, and, for a keyed provider, `staging.env.example` and `allowed_envs` |

Settings that live in GitHub variables can change without a pull request, a review, or a history.
Yet they fix the money-relevant RPC endpoints, the backup prefix, and the admin key. Today's values
are consistent: the `staging` Environment's 13 variables match the compose attested by the last
topup deploy (run 36670873413). The problem is the model, not a current mistake.

## 3. Principle: four kinds of input, each with one home

| Kind | Home | Attested | Examples |
|---|---|---|---|
| Topology: which services exist, what they mount, who talks to whom | `deploy/compose.yaml`, `deploy/compose.restore-check.yaml` | yes | uid/gid, the tmpfs keys, the dstack socket mounts, the smokescreen policy, `--webhook-proxy`, ports |
| Environment settings: the public values of one deployment | an environment directory: `compose.yaml` overlay and `topup.yaml` | yes | origin, admin key, RPC URLs, routes, WAL-G location, ingress domain, keyed providers' secret names |
| Deploy-time facts: known only from a release or a CVM | `render.sh` flags, format-checked and variant-scoped | yes | image digests, gateway domain, a restore instance's origin |
| Secrets | the CVM's sealed env | names only | S3 credentials, `SENTRY_DSN`, `TOPUP_RPC_<ID>_KEY` |

The sealed names are the `${…}` references of the rendered artifact: the base compose declares the
common ones, and the environment overlay declares its keyed providers' keys. There is no separate
list to keep in sync. `allowed_envs`, the unsealed env file Deploy sends, and preflight all read the
names from the artifact.

Deploy resolves the environment directory as `deploy/environments/${GITHUB_REPOSITORY_OWNER,,}/<env>`.
A fork therefore finds no directory and fails closed, instead of deploying Phala's admin public key
and domain. `render.sh` and preflight take any directory. `deploy/environments/example/` has no
Phala value, and preflight refuses its placeholders.

## 4. Target surface

### Files

```text
deploy/
  compose.yaml                   every service of a topup CVM (topology; pointers into docs)
  compose.service.yaml           variant: !reset null restore-check
  compose.restore-check.yaml     variant: !reset null dstack-ingress, smokescreen, heartbeat, backup;
                                 read-only topup on 8081; archiving off; read-only storage
                                 credentials under their own names
  compose-policy.jq              the policy of the merged artifact (§1.6)
  render.sh                      pinned compose config + the three deploy-time inputs
  pinned-compose.sh              Docker Compose v2.26.0 by sha256, cached; --no-download for offline use
  product/compose.yaml           reference-product stack
  environments/
    example/topup/               a generic environment: compose.yaml + topup.yaml, no Phala values
    phala-network/staging/topup/ compose.yaml (ingress DOMAIN, WAL-G location, RPC key names) + topup.yaml
    phala-network/staging/product/ compose.yaml (ingress DOMAIN) + config.json (every value literal)
  local/environment.sh           writes the local stacks' environment: staging's routes under a local
                                 header (placeholder providers, a local admin key)
```

Deleted:

- `docker-compose.yml`, `render-compose.sh`, `render-app-compose.sh`, `app-compose.example.json`.
- `staging.env.example` and `product/staging.env.example`: the sealed names come from the
  artifact.
- `write-staging-env.sh`: Deploy writes the unsealed file from `config --variables`.
- `config/routes/*.yaml`, moved into `topup.yaml`.
- `product/render-compose.sh` and `sandbox/render-sepolia-compose.sh`: the sandbox is an
  environment directory.
- `local/compose.sh`: local stacks merge the source files directly.

`check-route-modes.sh` stays, bound to the environment Deploy selected (§8).

### `topup.yaml`

```yaml
environment: staging                      # the Sentry environment (a tag only; `-restore` when --read-only)
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

`Config::parse` is the one validation path. It uses serde with `deny_unknown_fields`, as routes
do, and every rule below is a Rust test:

- the origin parses (`PublicOrigin`), and the admin key parses (`VerificationKey`);
- provider ids are lowercase letters, digits, and `-`;
- every provider URL is `https`, or `http` only when the origin is `http` (local stacks);
- a `{key}` fills a whole path segment or a whole query value, at most once;
- every route validates and the route set agrees across routes;
- every provider a route names is defined, and none is unused;
- a provider serves one chain, and one chain's providers are different URLs.

`topup config check FILE` and `topup config show FILE` (resolved JSON) run that path with no secret.
`topup config check --secrets FILE` also resolves each provider's key from `TOPUP_RPC_<ID>_KEY`,
with the same function `topup run` uses, and prints no value. `topup route validate|show` stay for
single route files (`examples/`, the sandbox template).

### Commands and flags

| Command | Config and flags |
|---|---|
| `topup run` | `--config FILE [--bind] [--webhook-proxy URL] [--read-only [--public-origin URL] [--restore-report FILE]]`; `--public-origin` and `--restore-report` require `--read-only` |
| `topup restore-check` | `--config FILE [--report FILE] [--expected-heartbeat-at … [--expected-lsn …]]` |
| `topup reconcile` | `--config FILE` |
| `topup config check`, `config show` | `FILE [--secrets]` (`check` only) |
| `topup migrate`, `restore-check` | `DATABASE_URL`, refused unless the login owns the database (or is a superuser) |
| `topup heartbeat`, `keys`, `healthcheck`, `attest`, `route …` | unchanged but for the removed mode variable |

### Rendering

```sh
deploy/render.sh --images images.json --gateway-domain gateway.dstack-pha-prod5.phala.network \
  deploy/environments/phala-network/staging/topup >docker-compose.staging.yml
deploy/render.sh --restore-check --images images.json --origin https://<app_id>-8081.<gateway> \
  deploy/environments/phala-network/staging/topup >restore-check.yml
deploy/render.sh --images images.json --gateway-domain … deploy/environments/phala-network/staging/product
```

Rendering runs in three steps:

1. The pinned Compose merges the files: `-p dstack --project-directory deploy -f STACK -f ENV/compose.yaml
   -f VARIANT config --no-interpolate --format json`. Here `STACK` is `compose.yaml`, or
   `product/compose.yaml` (with no variant) when the directory has a `config.json`. This keeps the
   secret references and `$$` escapes. Merged environments come out as `KEY=VALUE` lists, and
   render.sh turns them back into maps.
2. `jq` applies the inputs:
   - pins each `phala-pay`, `postgres-walg`, and `phala-pay-reference-product` image to the
     release's `repository@sha256`, and fails on any unpinned image;
   - sets dstack-ingress's `GATEWAY_DOMAIN`, or appends `--public-origin` to the read-only topup;
   - inlines each `file:` config as `content`, with `$` escaped, named `<name>_<sha256[:12]>`, and
     renames every service's `configs[].source` to match.
3. `config --no-interpolate` produces canonical YAML, and `compose-policy.jq` checks the result
   before it is printed.

Formats: `--images` is a JSON object of image name to `repository@sha256:<64 hex>`. `--gateway-domain`
is a lowercase host name. `--origin` is an `https://` origin, restore-check only.
`--project-name` is for local rehearsals only; the CVM's project is `dstack`.

Verified on v2.26.0 and v5.5.1:

- `--no-interpolate` keeps `${S:-}` and `$$`;
- `!reset null` removes a service;
- an inactive-profile service stays in the output, with its `profiles:`;
- both versions produce byte-identical output, which v2.26.0 loads.

The output bakes the project name into resource names (`name: dstack`, `dstack_pgdata`), and these
are exactly the names the CVM derives today from `/dstack`. §11 proves that the upgrade keeps the
data.

## 5. Every setting, before and after

| Setting | Today | After |
|---|---|---|
| `TOPUP_IMAGE`, `POSTGRES_WALG_IMAGE`, `PRODUCT_IMAGE` | `${X_IMAGE:-zero digest}` ×10, rewritten by awk | `image: phala-pay` etc.; `render.sh --images` pins them |
| `TOPUP_IMAGE` in topup's env (Sentry release) | env | removed; the release is the source commit, compiled in (`SOURCE_COMMIT` build arg beside `SOURCE_DATE_EPOCH`) |
| dstack-ingress image | pinned digest | unchanged |
| `TOPUP_DOMAIN` | GitHub var, rendered twice | overlay `dstack-ingress.environment.DOMAIN` and `topup.yaml` `public_origin`; the policy checks they agree |
| `TOPUP_PUBLIC_ORIGIN` | env `https://${TOPUP_DOMAIN}` | `topup.yaml` `public_origin`; restore-check: `render.sh --origin` → `--public-origin` |
| `TOPUP_GATEWAY_DOMAIN` | read from the CVM, rendered | unchanged source; `render.sh --gateway-domain` |
| `TOPUP_ADMIN_KID` | derived `admin/<env>-v1`, or a variable | `topup.yaml` `admin_key.id`, explicit |
| `TOPUP_ADMIN_PUBLIC_KEY` | GitHub var | `topup.yaml` `admin_key.public_key` |
| `TOPUP_RPC_<ID>_URL` | GitHub vars → `x-rpc-providers` → env of 2 services | `topup.yaml` `rpc_providers` |
| `TOPUP_RPC_<ID>_KEY` | sealed; name in 3 lists | sealed; declared once in the environment overlay (staging keeps its two names, so no re-seal) |
| `{key}` placement | anywhere; the key is substituted as text | a whole path segment or query value only; the authority must survive substitution (P1 fix) |
| Routes | 4 files + 4 compose copies + 8 mounts + 8 `--route` args | `topup.yaml` `routes`; one config, mounted in 2 services |
| `WALG_S3_PREFIX`, `AWS_ENDPOINT`, `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE` | GitHub vars, or derived for R2 | overlay literals (WAL-G's env interface), explicit |
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | sealed; the restore instance's are the same names | sealed; the restore-check variant reads `RESTORE_AWS_ACCESS_KEY_ID`/`RESTORE_AWS_SECRET_ACCESS_KEY`, so an instance created without `--env-file` inherits no read-write credential |
| `SENTRY_DSN` | sealed | unchanged |
| `SENTRY_ENVIRONMENT` | derived from the Environment's name | `topup.yaml` `environment` (a Sentry tag, never a policy input) |
| `TOPUP_SERVICE_ENABLED` (`on`/`read-only`/`off`) | set by the renderer in 2 services | `topup run --read-only` in the variant; `heartbeat` does not exist there; `off` is removed (only a test used it) |
| `TOPUP_RESTORE_FROM_BACKUP` | set by the renderer in 3 services | `postgres` only, `"on"` in the variant (the entrypoint defaults to `off` and turns archiving off); `restore-check` no longer reads it, since it exists only in its variant; `walg-cron` keeps its guard |
| `TOPUP_WEBHOOK_PROXY` | env | `--webhook-proxy`, beside the smokescreen service it names |
| `TOPUP_RESTORE_REPORT_FILE` | env in 2 services | `--restore-report` (run) and `--report` (restore-check) |
| `TOPUP_BACKUP_TIMESTAMP_FILE` | env in 3 services, always the default | removed |
| `MIGRATE_DATABASE_URL` / `DATABASE_URL` | 2 names | `DATABASE_URL`; `migrate` and `restore-check` check in code that the login owns the database |
| `PGPASSFILE` | env | unchanged (libpq) |
| `heartbeat --interval-s 60` | flag | removed (the default) |
| `phala-pay.rendered-sha256` label | on every service | removed; config names carry their digest (§6) |
| `allowed_envs` | `app-compose.example.json` | the artifact's `${…}` names |
| `PHALA_WORKSPACE`, `TOPUP_CVM_ID`, `STAGING_PRODUCT_CVM_ID`, secret `PHALA_CLOUD_API_KEY` | GitHub | unchanged: deployment state and tool credentials, not attested |
| `PRODUCT_DOMAIN`, `PRODUCT_DRIVER_PUBLIC_KEY`, `TOPUP_ORIGIN`, `PRODUCT_PUBLIC_URL` | GitHub vars and derivations | the product overlay's `DOMAIN` and literal values in `config.json` |

After the change:

- The `${…}` names in the service artifact go from 23 to 5, the secrets only.
- GitHub Environment variables go from 13 (+3 optional) to 3.
- The fixed environment variables `topup` reads go from 15 to 2 (`DATABASE_URL`, `SENTRY_DSN`),
  plus each `TOPUP_RPC_<ID>_KEY`. libpq reads `PGPASSFILE`.
- Adding a chain touches the environment directory only.

**Where a secret may appear.** `compose-policy.jq` allows a `${NAME:-}` reference only as the whole
value of an environment key it names:

| Secret | Allowed positions |
|---|---|
| `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` | `postgres`/`backup` env of the same key (service variant) |
| `RESTORE_AWS_*` | `postgres` `AWS_*` (restore-check variant) |
| `SENTRY_DSN` | `topup` `SENTRY_DSN` |
| `TOPUP_RPC_<ID>_KEY` | `topup`/`restore-check` env of the same key |
| `PRODUCT_API_KEY` | `product` `PRODUCT_API_KEY` |

Once every `$$` is removed, no other `$` may remain anywhere: not in a config's content, a command,
or another key. A sealed value therefore cannot fill the admin key, the origin, or an RPC host.

## 6. Services

| Service | Decision | Evidence |
|---|---|---|
| `keys` | stays | See the paragraph below the table. |
| `migrate` | stays | A one-shot init job gated by `service_completed_successfully` is the Compose idiom. Folding it into topup would hand topup the owner login. |
| `heartbeat` | stays, service variant only | Its row is the RPO anchor (`restored_heartbeat_at` within 120 s of the failure point) and the steady commit that keeps WAL flowing. `topup run` checks the contracts over RPC before it touches the database, waits for the lease-owner lock, and exits on any task failure. Folded into it, an RPC outage or a crash loop would stop the heartbeat and produce false RPO findings. |
| `smokescreen`, `backup` | service variant only | They idle in restore-check today. |
| `restore` (profile `tools`) | moves to `local/restore-drill.compose.yml` | Only `restore-drill.sh` runs it; it is attested today but never started on a CVM. |
| `rendered-sha256` label | replaced | See the paragraph below the table. |

**`keys`.** The Postgres and WAL-G images cannot call dstack, so something must derive their
passwords and backup key into tmpfs. The separation this buys is by mount and by database role:
`postgres`, `backup`, and `migrate` see only the credential files they mount, and topup logs in as
`topup_service`, never as the owner. It is not KMS isolation. `keys`, `topup`, `restore-check`,
and dstack-ingress mount the dstack socket, and any socket holder can derive any path, so a
compromised `topup` could derive the owner password.

**The `rendered-sha256` label.** Compose v2.26.0 hashes only the service definition
(`pkg/compose/hash.go`) and copies config content only at container creation
(`createMobyContainer` → `injectConfigs`). A content-only change would therefore keep the old file,
and some trigger is still needed. Naming each config after its digest is Compose's documented
config rotation practice (kustomize hashes ConfigMap names the same way). The services'
`configs[].source` references change with the name, so exactly the services that mount a changed
config have a new definition. v2.26.0 also force-recreates every service that `depends_on` a
recreated one (`convergence.go:538`, `setDependentLifecycle`). The goal is therefore scoped: a
route change recreates `topup` and its dependent dstack-ingress, but no longer the Postgres
container. On a CVM an upgrade restarts the whole stack anyway, since `app-compose.service` stops
it, so recreation is about container identity, not uptime. §11 measures the actual lifecycle.

## 7. Alternatives rejected, with evidence

- **`profiles` as the variant switch.** The switch is runtime state (`--profile`/`COMPOSE_PROFILES`)
  outside the attested file, and dstack passes neither. Compose v2.26 also keeps an inactive
  profile's service in `config` output, so the artifact would still carry it.
- **`include`.** It imports whole projects and cannot modify an existing service.
- **Compose `secrets:` with `environment:` sources.** v2.26.0 writes a secret only when it creates
  the container. After `phala envs update` (a restart, with an unchanged hash) every container would
  keep the old secret. Tested.
- **`env_file:` with dstack's decrypted env.** It would hand every sealed secret to every service
  that lists it.
- **Interpolating public settings at render time.** `config` would resolve the secret references
  too, and `$${…}` escapes survive as literals. Verified.
- **Two complete compose files.** They duplicate ~200 lines that must then be proved equal.
- **Routes as separate files next to `topup.yaml`.** That keeps 4 configs and mounts, and makes the
  renderer discover files.

## 8. Scripts and workflows after the change

- **Every config consumer switches to `--config`:** `topup run`, `reconcile`, and `restore-check`
  in the composes, the local stacks, the sandbox, the drill, and the rehearsals. `cargo test` runs
  `Config::parse` on every committed `topup.yaml` (example and staging).
- **`compose-policy.jq`** (§1.6) is included by `validate-compose.sh`, preflight, and
  `verify-attestation.sh`.
  - Service variant: exactly `keys postgres migrate topup smokescreen dstack-ingress heartbeat
    backup`; only dstack-ingress publishes, on 443 (tls-alpn-01 → `topup:8080`, `DOMAIN` equal to
    `public_origin`'s host); the credential mounts; smokescreen unrelaxed.
  - Restore-check variant: exactly `keys postgres migrate topup restore-check`; only topup
    publishes, on 8081 → 8080; topup `--read-only`; postgres with `TOPUP_RESTORE_FROM_BACKUP=on`
    and no command or entrypoint override (so `archive_mode=off` wins); storage credentials only
    `RESTORE_AWS_*`.
  - Both variants: no password in env; the secret positions of §5; volumes named `<project>_*`.
- **`validate-compose.sh`** renders the example environment, the staging environment, and the
  product in both variants, and applies the policy. It also checks the local, drill, sandbox, and
  rehearsal overlays.
- **`preflight.sh`** takes `--env FILE`, `--compose FILE`, and `--environment-dir DIR`.
  - `--offline` makes no network access and pulls nothing. It checks:
    - the env file names against the artifact's names;
    - that images are pinned;
    - a byte-equal fresh render with the pinned Compose (`--no-download`);
    - the policy;
    - that no example placeholder remains;
    - `topup config check [--secrets]` in the pinned image, which must already be present
      (`--pull never`); otherwise it fails and names the `docker pull`.
  - Online, it adds the anonymous pulls, RPC and asset checks from `topup config show` JSON,
    and the Phala Cloud checks.
  - `--unsealed` skips only `--secrets`.
- **`check-route-modes.sh`** stays. It reads the routes from the artifact's `topup` config, and the
  environment is the one Deploy selected: its first argument. It never reads `topup.yaml`'s
  `environment`, so a staging config that claims `production` does not bypass it. It needs no
  network.
- **`deploy.yml`** loses the settings `env`, the RPC copy step, and the derivations. It resolves the
  environment directory from the owner and Environment, renders, writes the unsealed env from the
  artifact's names, and runs the same verification as before.
- **`release-images.yml`** writes `images.json` keyed by image name, builds with `SOURCE_COMMIT`,
  and runs `render.sh`'s image check.
- **`verify-contracts.yml`** takes Sepolia's two provider URLs from the committed staging
  `topup.yaml` (`topup config show`). It fails if one needs a `{key}`, since the workflow holds no
  key. It validates that config instead of the deleted route directory. It no longer needs the
  `staging` Environment.
- **Local stacks** (`make up`, the sandbox, the drill) render a local environment with
  `render.sh --project-name <their project>`. That environment is `local/environment.sh`'s, or
  the sandbox's or drill's own. The configs are therefore inline, and nothing is bind-mounted on
  CI's runner. They then add the local overlay and its per-variant part (`local/service.yml` or
  `local/restore-check.yml`). The CVM rehearsal and the upgrade rehearsal render the same way.
- **Docs** move the compose's design prose out; the compose keeps one-line pointers. Affected:
  - `deploy/README.md`;
  - `RESTORE.md` (render commands; the `RESTORE_AWS_*` names);
  - `phala.md`;
  - `docs/self-hosting.md` (any environment directory; picking and verifying the official
    digests);
  - `docs/configuration.md`, rewritten around `--config`;
  - architecture §14;
  - the `TOPUP_PUBLIC_ORIGIN` wording in the admin OpenAPI.

## 9. Migration

**Phala's staging.** One Deploy `upgrade`; the sealed names are unchanged and no data moves.

1. `deploy/environments/phala-network/staging/` holds the current `staging` Environment values
   (owner decision). They match the compose attested by run 36670873413, and the §11 diff proves
   it.
2. **Before the cutover** (HUMAN-ONLY, owner):
   - save the currently attested artifact and its verification;
   - run `deploy/verify-attestation.sh` on it, and record the current compose hash;
   - record the TLS evidence;
   - record the newest `base_…` backup and `wal_005/` segment.
3. **Cutover**: merge, run Release images (the binary changed), then Deploy `upgrade` for
   `topup`. The new compose hash is a new full attested app-compose hash
   (`verify-attestation.sh`), not a YAML file hash. It goes through the merchant approval flow like
   any upgrade: announce it with the commit, and let merchants re-pin with
   `docs/integration.md` §5.3. The webhook keys are unchanged, since they are derived from the
   same app id.
4. **Acceptance**, from the Deploy run and the owner's checks:
   - the attestation verifies, and the policy holds;
   - `/healthz` answers;
   - the TLS evidence verifies;
   - admin `GET /v1/admin/restore` shows no new restore;
   - a pre-upgrade account and its webhook key are unchanged (`GET /v1/attestation`);
   - a new `wal_005/` segment is listed within two minutes;
   - `topup-backup` checks in `ok`.
5. **Product**: Deploy `upgrade` with target `product`; its `ledger` volume keeps its name.
6. **Rollback.** No schema change is in this PR, so the old binary runs on the same database.
   - The standard path: revert the merge on `main`, run Release images, and Deploy `upgrade`.
   - The emergency path (HUMAN-ONLY, owner): redeploy the saved artifact byte for byte with
     `phala deploy --cvm-id "$TOPUP_CVM_ID" --compose <saved> --no-public-logs --no-public-sysinfo
     --wait`, with no `-e`, so the sealed env stays. Then verify it with the old commit's
     `verify-attestation.sh` (its four-argument form), against the saved hash. The new policy
     judges the new artifact form only. Its images stay in GHCR.
7. **Afterwards**, delete the unused `staging` variables: `TOPUP_DOMAIN`, `AWS_ENDPOINT`,
   `WALG_S3_PREFIX`, `TOPUP_ADMIN_PUBLIC_KEY`, the four `TOPUP_RPC_*_URL`, `PRODUCT_DOMAIN`, and
   `PRODUCT_DRIVER_PUBLIC_KEY`. Nothing reads them any more.
8. **A restore after the cutover** uses `RESTORE_AWS_*` in its env file (`RESTORE.md`).

**Self-hosters (forks):**

1. Copy `deploy/environments/example/` to `deploy/environments/<owner>/<env>/`.
2. Fill it with the values from their GitHub variables, and move each keyed provider's key name
   into the overlay.
3. Run Release images and Deploy `upgrade`. Their sealed env is untouched. Deploy fails closed
   until the directory exists.

**The Phala Cloud template** (Phala-Network/phala-cloud#520) needs a follow-up there: the rendered
service compose, with `topup.yaml` as an inline config filled from the template's form.

## 10. Self-hosting without a fork

**In this PR**:

- `render.sh` and preflight accept any environment directory;
- the example environment;
- keyed providers' secret names in the overlay;
- docs on picking an official release:
  - the source commit of a Release images run on `main`;
  - its `images.json` digests;
  - `make verify-image` at that commit to rebuild and compare, since `phala-pay` is bit-for-bit
    reproducible.

**Follow-up**:

- versioned releases (tags and notes);
- a deploy kit consumed at a tag: the composes, `render.sh`, and a reusable `workflow_call` Deploy
  or a small CLI;
- the Cloud template.

Mixing those into this PR would put the live staging migration and a new distribution model at
risk together.

## 11. Verification

**The in-place upgrade rehearsal** (`deploy/local/upgrade-rehearsal.sh`, `make upgrade-rehearsal`)
proves zero data loss on real committed data. It is the local equivalent of the staging cutover.

1. **The old stack.** It starts from staging's old images (the digests of run 36670873413, pulled
   anonymously) and the old compose: `git show <base>:deploy/…` rendered by the old renderer. It
   runs Anvil with the factory, the local S3, and the simulator, with sealed secrets.
2. **Real data.** It creates an account through the signed admin API, with its webhook endpoint,
   its treasury proof, a deposit address, and a quote. Then it records the evidence:
   - webhook key (attestation);
   - row counts;
   - `pg_control_system()` system identifier;
   - timeline and the migrations;
   - the newest archived segment.
3. **Pre-upgrade assertions**:
   - project name;
   - every volume name and each service's mount targets, including `ingress_certs`,
     `ingress_evidences`, `observability`, and the product's `ledger`;
   - `PGDATA`;
   - the simulator's app id;
   - the sealed names equal the new artifact's;
   - backup prefix;
   - the derived key paths;
   - the old artifact is saved.
4. **The upgrade.** It builds the new images, renders the new artifact with `render.sh` under the
   same project name, and runs `docker compose up` on the same volumes, as the CVM does.
5. **Post-upgrade assertions:**
   - the same system identifier and timeline;
   - every pre-upgrade row present;
   - the migrations the same or ahead;
   - the same webhook key and deposit address for the same customer;
   - a new segment archived after the upgrade;
   - the new attestation (simulator quote) served;
   - the Postgres logs show neither "restoring base backup" nor "initializing a new cluster", and
     no `recovery.signal` exists;
   - `topup` reports no restore (`GET /v1/admin/restore`).
6. **Config-only change lifecycle.** A changed route recreates `topup` and dstack-ingress's
   definition, but not Postgres: the Postgres container id is the same.
7. **Rollback.** It redeploys the saved old artifact on the same volumes and asserts the same data
   again.

TLS evidence needs a real CVM and domain. Deploy verifies it on the staging cutover (§9); the
rehearsal asserts that the `ingress_certs` volume is unchanged.

**Other checks:**

- **Behavioural equivalence.** Render the old staging artifact with run 36670873413's inputs and
  the new one with the same images and gateway. Normalize both with the pinned Compose, then diff.
  Every difference is listed and explained in the PR.
- **Suites:** `validate-compose.sh`, `preflight.sh --offline` against the rendered staging
  artifact, the CI `rust`, `deployment`, and image jobs (`cargo test`, `make lint`, the shell
  tests), `make restore-drill` (both modes), and `make cvm-rehearsal`.
- **Limits:** `CARGO_BUILD_JOBS=8`. Every container, volume, image, and temp file of a run is
  removed.

## 12. Risks

- **The artifact's form changes** (canonical YAML, no comments, explicit resource names).
  `verify-attestation.sh` and the runbooks are updated. Merchants pin only the compose hash
  (`docs/integration.md` §5.3).
- **A new pinned tool.** `render.sh` refuses any Compose but v2.26.0 by sha256.
- **Volume identity.** Rendering under any project name but `dstack` would move the CVM onto new
  empty volumes. `render.sh` fixes `-p dstack`, the policy asserts the volume names, and the
  rehearsal proves the data survives.
- **The restore credential names change.** A restore env file with the old names fails closed:
  PostgreSQL cannot list the prefix. `RESTORE.md` and its preflight name the new ones.
- **Moving validation into Rust** changes where errors surface. The rules are carried over one for
  one, with tests.

## 13. What stays, and why

- Attestation of every public setting that affects money or trust. Each one is in `topup.yaml`,
  an overlay, or a pinned image, all inside the one attested file.
- Sealed secrets and `allowed_envs`. The admin seed never enters the CVM.
- `keys`, the three tmpfs volumes and their mount policy, and the pinned dstack-ingress
  (tls-alpn-01, 443 only).
- The smokescreen policy; WAL-G, its key path, and its guards; `postgres-init`.
- Deploy's two modes, its read-back and verification; preflight online and offline.
- Every guarantee of `RESTORE.md`, now checked on the merged artifact. The drill keeps all of its
  assertions: the object listing is unchanged, live isolation holds, merchant requests are refused,
  background work stops, and the freeze persists after the upgrade back to the service.
- YAML; `<owner>/<env>`; the three bounded deploy-time inputs; digest-suffixed configs; `!reset
  null` overrides.
