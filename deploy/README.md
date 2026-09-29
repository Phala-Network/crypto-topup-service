# dstack deployment

How an operator deploys and runs its own Phala Pay instance. Phala Pay is self-hosted: each
operator deploys from its own fork, to its own Phala Cloud workspace, on its own domain; the
[self-hosting guide](../docs/self-hosting.md) is the order of the steps, and this document their
reference. Names in `$VARIABLES` are the operator's own values; Phala's are given only as labelled
examples, and the demo that only Phala runs is in [Phala's instance](#phalas-instance).

Every CVM is deployed by GitHub Actions from the fork's `main`; nothing is deployed from a laptop.
Steps outside the workflows that change a registry, Phala Cloud, a CVM, a Safe, a contract, DNS, or
a secret are marked **HUMAN-ONLY**. Backup and restore: [RESTORE.md](RESTORE.md). Incident response:
[runbooks](runbooks/README.md). Contracts: [CONTRACTS.md](CONTRACTS.md).

## What runs where

| Where | What | Deployed by |
|---|---|---|
| topup CVM, one per Environment (`staging`, `production`) | [docker-compose.yml](docker-compose.yml): `keys` (derives the database passwords and the backup key), `postgres` (PostgreSQL 18 + WAL-G), `migrate`, `topup` (the service, or read-only and published on 8081 in the [restore-check variant](RESTORE.md#the-restore-check-variant)), `smokescreen` (the [webhook egress](#webhook-egress) proxy), `dstack-ingress` (the only public port, 443: TLS for the [custom domain](#custom-domain), service variant only), `heartbeat`, `backup`, `restore-check` (acts only in that variant) | Deploy, target `topup` |
| Staging reference-product CVM (Phala's demo; optional) | [product/docker-compose.yml](product/docker-compose.yml): `product` (on 8089, private) and `dstack-ingress` (the only public port, 443: TLS for its [custom domain](#custom-domain)) | Deploy, target `product` |
| Object storage (S3-compatible; Phala's instance: Cloudflare R2) | encrypted WAL-G base backups and WAL under `WALG_S3_PREFIX` | owner |
| The operator's Sentry project (optional; Phala's: `phala-network/crypto-topup-service`) | errors, alerts, Crons and Uptime monitors | the service itself |
| EVM chains of the routes (the committed routes: Sepolia) | the permissionless forwarder factory, at one deterministic address on every chain; forwarders; each account's own treasury | factory: any deployer ([CONTRACTS.md](CONTRACTS.md)); treasuries: each merchant, through the API |
| GitHub Actions | [Release images](../.github/workflows/release-images.yml), [Deploy](../.github/workflows/deploy.yml), [Verify contracts](../.github/workflows/verify-contracts.yml) (daily, read-only), [Restore drill](../.github/workflows/restore-drill.yml) (weekly, local stack) | — |

Production CVMs have no SSH, no logs, and no database access. Everything an operator sees comes
from the public endpoints, the admin API, Sentry, and the chain.

### KMS

Every CVM uses Phala Cloud's KMS (`--kms phala`, owner decision): no `DstackApp` contract and no
on-chain compose-hash approval. Fund safety does not depend on upgrade governance: the service
holds no key that moves funds and sends no transactions, and a forwarder pays only the treasury
its address commits to. Credits are what the attested service signs with each account's webhook
key, and merchants pin their keys from attestation and may cap or verify credits on their own
node. A malicious upgrade could cause downtime, read service data, or sign false credits within
what each merchant's own limits accept; the compose hash in the attestation, verified after every
deploy, makes it detectable.

### OS image

The approved OS image is `dstack-0.5.9`, non-dev: the latest dstack release a Phala Cloud node
offers. [preflight.sh](preflight.sh) accepts only that name and, online, requires a node of the
workspace to offer it. The service speaks the dstack 0.5 guest API (`dstack-sdk = "=0.1.3"`); the
local simulator is built from the same release. dstack 0.6 derives different keys for the same
domain, so moving to it changes every account's webhook keys (which merchants pin), the backup
key, and the database passwords: that is a key migration, not an image bump.

## One-time setup (HUMAN-ONLY, repository owner)

0. **Fork.** Fork the repository and enable Actions on the fork. Release images publishes to the
   repository owner's namespace, `ghcr.io/<owner, lowercased>/` (Phala's: `ghcr.io/phala-network/`),
   the only one its `GITHUB_TOKEN` can push to, so a fork changes nothing; its packages are made
   public in step 6. Workflows run on `ubuntu-latest` unless the repository variable `CI_RUNNER`
   names another runner.
1. **Environments.** Repository Settings > Environments: `production` and, for a pre-production
   instance with test routes only, `staging` (the names Deploy offers), deployment branches `main`
   only. Required reviewers are optional (Phala's repository has none: its plan does not offer
   them). Whoever dispatches Deploy is accountable; the run's actor, summary, and uploaded record
   are the audit trail.
2. **Phala Cloud.** Create an API key for the Environment's workspace and store it as the
   Environment secret `PHALA_CLOUD_API_KEY`, the only secret GitHub holds.
3. **Object storage.** Create a bucket (or prefix) per Environment in an S3-compatible store, such
   as Cloudflare R2, that the other Environment's keys cannot reach, and a read-write API token for
   it. The token is sealed into the CVM (below), never stored in GitHub.
4. **Environment variables**, per Environment:

   | Name | Value |
   |---|---|
   | `PHALA_WORKSPACE` | display name of the API key's workspace (preflight checks it) |
   | `TOPUP_CVM_ID` | empty until the first provisioning, then the CVM id from the run summary |
   | `TOPUP_DOMAIN` | the [custom domain](#custom-domain), a name in the operator's DNS such as `pay-api.example.com` (Phala's instance: `pay-api-staging.phala.com` in `staging`, `pay-api.phala.com` in `production`) |
   | `AWS_ENDPOINT` | the object store's endpoint, for R2 `https://<account>.r2.cloudflarestorage.com` |
   | `WALG_S3_PREFIX` | `s3://BUCKET/PATH`; a new app needs a prefix of its own ([RESTORE.md](RESTORE.md#bootstrap-from-backup)) |
   | `TOPUP_ADMIN_PUBLIC_KEY` | from `topup-sdk keygen --keyid admin/<Environment>-v1`, a separate key per Environment; the seed stays with the admin |
   | `TOPUP_RPC_<ID>_URL`, one per [RPC provider](#rpc-providers) of the compose | the provider's HTTPS RPC URL for its chain, with `{key}` in place of an API key; the Sepolia routes use `TOPUP_RPC_PROVIDER_A_URL` and `TOPUP_RPC_PROVIDER_B_URL`, the Base Sepolia routes `TOPUP_RPC_BASE_SEPOLIA_A_URL` and `TOPUP_RPC_BASE_SEPOLIA_B_URL` ([Staging routes](#staging-routes)) |
   | `STAGING_PRODUCT_CVM_ID`, `PRODUCT_DRIVER_PUBLIC_KEY` | `staging` only, for the optional [staging reference product](#staging-reference-product) |
   | `PRODUCT_DOMAIN` | `staging` only, likewise: the reference product's [custom domain](#custom-domain) (Phala's: `pay-demo-api.phala.com`; the [website](#website), `pay.phala.com`, is on Cloudflare) |

   No variable or secret names a treasury or a transaction-signing key: treasuries are each
   account's own, set through the API, and the service sends no transactions.

   That is six variables and one per RPC provider (eight with Sepolia's two) for the service, and
   three more for the staging reference product.
   All but the first two are [attested settings](#attested-settings). Deploy derives the rest, and
   a variable of the same name overrides a derived value where noted:

   | Setting | Derived as |
   |---|---|
   | `SENTRY_ENVIRONMENT` | the Environment's name (no override) |
   | `TOPUP_GATEWAY_DOMAIN` | `gateway.<base domain>` of the CVM's node, read from the existing CVM on `upgrade`; `provision` renders a provisional value and upgrades the new CVM once its node is known (no override) |
   | OS image | `dstack-0.5.9`, fixed in `deploy.yml` (architecture §14; no override) |
   | `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE` | `auto` and `true` for an R2 `AWS_ENDPOINT`; set both variables for any other endpoint |
   | `TOPUP_ADMIN_KID` | `admin/<Environment>-v1`; set the variable only after an admin key rotation to a new key id |

   `TOPUP_PUBLIC_ORIGIN` is not a variable: the compose sets it to `https://$TOPUP_DOMAIN`.
5. **Sentry** (project admin; the Crons monitors create themselves on their first check-in):
   - Settings > Security & Privacy: keep *Data Scrubber* and *Use Default Scrubbers* on; turn
     *Prevent Storing of IP Addresses* on.
   - An alert for the environments `staging` and `production` (not `*-restore`) that notifies the
     on-call owner when an issue is created or regresses, with no level filter (most alert lines are
     `warning` events). Confirm that the Crons and Uptime monitors are listed as connected.
   - One Uptime monitor per Environment (UI only): `GET https://<TOPUP_DOMAIN>/healthz`, interval
     1 minute, timeout 10 seconds, environment = the Environment's name.
   - The project DSN (Settings > Client Keys) is sealed as `SENTRY_DSN` ([Sealing the
     secrets](#sealing-the-secrets)).
6. **Packages.** After the first Release images run, make `phala-pay`, `postgres-walg`, and
   `phala-pay-reference-product` public (the owner's Packages > package > Package settings >
   Change visibility; an organization must allow public container packages). This is
   irreversible. CVMs pull without credentials, and preflight fails on a private image.

## Release and deploy

### Build and publish images

Run **Release images** on `main` (Actions tab, or `gh workflow run release-images.yml --ref
main`). It builds `phala-pay`, `postgres-walg`, and `phala-pay-reference-product`, checks
that the reproducible ones build bit for bit twice, pushes them tagged `sha-<12 hex commit>`, and
records the `repository@sha256` references in its summary and its `images.json` artifact.
`postgres-walg` is not bit-for-bit reproducible (apt and dpkg timestamps). The same two-build
check runs locally without pushing: `make verify-image`.

### Deploy

[Deploy](../.github/workflows/deploy.yml) is the source of truth for what a deployment checks and
does; its inputs are `environment`, `target` (`topup` or `product`), `mode` (`provision` or
`upgrade`), and `release_run_id`. It deploys only the digests of a successful Release images run
on `main`, runs preflight, verifies the attested compose with the official dstack verifier after
every deploy, and uploads the rendered compose and the verification as the run's record.

1. Merge the change to `main` and run Release images.
2. First deployment: Deploy with `mode: provision`, then set `TOPUP_CVM_ID` (or, for the product,
   `STAGING_PRODUCT_CVM_ID`) to the CVM id in the summary, [seal the
   secrets](#sealing-the-secrets), and create the [DNS records](#custom-domain) it lists. If a provision run fails after the summary shows a CVM id, set
   the variable, seal the secrets (an upgrade waits for `/healthz`), and re-run with `mode:
   upgrade` and the same release; never provision twice.
3. Every later change: Deploy with `mode: upgrade`. An upgrade sends only the compose, so the
   sealed env stays. Rollback is an upgrade to an earlier release; never roll a schema back, use a
   forward repair migration.

**Production** with live routes additionally needs the factory at its deterministic address on each
of their chains ([CONTRACTS.md](CONTRACTS.md#mainnet), HUMAN-ONLY, only where it is missing) and a
reviewed route PR putting the live routes into the compose. One production deployment serves both
modes: live routes on mainnets and test routes (`livemode: false`) on test networks such as Sepolia.
Deploy runs [check-route-modes.sh](check-route-modes.sh) on the rendered compose and refuses a route
whose `livemode` does not match its chain, a chain on neither of its lists, a local development
chain, and any live route in `staging`. After the first deploy, in order: seal the secrets; [verify
the attestation](#attestation-ingress-and-egress); have the operator's Finance, Risk, and Operations
approve the pilot limits (route bounds, each account's caps (`limits`: open quotes and their credit
per account and per customer) and `max_unfinalized_credit`; architecture §17) and a passed restore
drill ([RESTORE.md](RESTORE.md)); then [onboard](#operator-onboarding) Phala's own accounts with
`charges_enabled` (third-party merchants only after the legal review, design §17).

### Sealing the secrets

The CVM's encrypted env holds exactly the names of [staging.env.example](staging.env.example),
the same in both Environments: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` (the object store's token),
`SENTRY_DSN` (empty turns Sentry off), and a `TOPUP_RPC_<ID>_KEY` per [RPC provider](#rpc-providers)
that may take a key (`TOPUP_RPC_PROVIDER_A_KEY` and `TOPUP_RPC_PROVIDER_B_KEY` for Sepolia's): the
API key topup puts in place of `{key}` in the provider's attested URL, empty for a keyless URL (as
in Phala's staging). A key is at least 8 characters of `A-Z a-z 0-9 - . _ ~`; topup refuses a provider whose
URL and key disagree (a `{key}` without a key, or a key without a `{key}`). Redaction keeps the URL
and the key out of every log line and error. A new CVM waits for them: PostgreSQL initializes a cluster
only after listing an empty backup prefix ([RESTORE.md](RESTORE.md#bootstrap-from-backup)).

**HUMAN-ONLY, owner, from their own machine**, in a checkout of the deployed commit with the
rendered compose from the run's artifact (the provision summary prints these commands):

```sh
deploy/preflight.sh --env .env.ENV --compose docker-compose.ENV.yml --offline   # .env.ENV: mode 0600
npx --yes phala@1.1.22 envs update "$TOPUP_CVM_ID" -e .env.ENV
```

Deploy's preflight has no keys, so with a keyed provider it skips the asset chain checks; run
preflight without `--offline` (with `--workspace` and `--os-image`) to run them with the keys.

The CVM restarts, `/healthz` answers, and backups have started once a WAL segment younger than two
minutes is listed (`aws s3 ls "${WALG_S3_PREFIX%/}/wal_005/" --endpoint-url "$AWS_ENDPOINT" | tail -1`).
Re-seal the same way whenever a secret changes; never change a setting with `envs update`.

**A new sealed name** (a new keyed provider's `TOPUP_RPC_<ID>_KEY`, or a CVM sealed before the
RPC keys existed) changes the CVM's allowed names, and Deploy `upgrade` keeps a CVM's allowed
names, so its attestation check would refuse the upgrade. Once, before that upgrade, run only the
`envs update` above with `.env.ENV` holding every name of the new `staging.env.example` (the keys
empty for keyless URLs): the running compose ignores the new names, and preflight against it would
refuse them. A new keyless provider adds no name.

### Attested settings

A value in the encrypted env is outside the attestation: whoever can run `phala envs update` could
change it without changing the compose hash. So the env holds only the secrets above, and
[render-compose.sh](render-compose.sh) writes every other `${NAME:-}` of the compose inline from
the Environment variables, refusing a value that is not 1-512 printable ASCII characters without
spaces, quotes, backslashes, or `$`. The settings are public (the compose is in the attestation), so
an RPC provider's API key is not one: its URL has `{key}` where the key goes, and the key is sealed
([Sealing the secrets](#sealing-the-secrets)):

| Setting | Source |
|---|---|
| `AWS_ENDPOINT`, `WALG_S3_PREFIX`, `TOPUP_ADMIN_PUBLIC_KEY`, every `TOPUP_RPC_<ID>_URL` | the Environment variables of the same name |
| `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`, `TOPUP_ADMIN_KID`, `SENTRY_ENVIRONMENT` | derived ([One-time setup](#one-time-setup-human-only-repository-owner), step 4) |
| `TOPUP_DOMAIN`, `TOPUP_GATEWAY_DOMAIN` | the Environment variable, and the CVM node's gateway: `dstack-ingress`'s `DOMAIN` and `GATEWAY_DOMAIN`; topup's `TOPUP_PUBLIC_ORIGIN` is `https://$TOPUP_DOMAIN` (the product's `PRODUCT_DOMAIN` and `PRODUCT_GATEWAY_DOMAIN` likewise, with `PRODUCT_PUBLIC_URL` `https://$PRODUCT_DOMAIN`) |
| `TOPUP_IMAGE`, `POSTGRES_WALG_IMAGE` | the release's digests; the image digest is also the Sentry release |
| `TOPUP_RESTORE_FROM_BACKUP`, `TOPUP_SERVICE_ENABLED` | the variant: service `off`, `on`; `--restore-check`: `on`, `read-only` |
| ingress | the variant: the service runs `dstack-ingress` on 443 and publishes no topup port; `--restore-check` runs no ingress and publishes topup on 8081 (blocks after `# only-in: VARIANT` in the compose) |
| `dstack-ingress` image | pinned in [docker-compose.yml](docker-compose.yml) by digest ([Custom domain](#custom-domain)) |
| route files (inline configs) | committed in [docker-compose.yml](docker-compose.yml), checked against `config/routes/` by [validate-compose.sh](validate-compose.sh) |
| RPC provider ids | the `x-rpc-providers` block of [docker-compose.yml](docker-compose.yml) ([RPC providers](#rpc-providers)) |

Every service also carries the label `phala-pay.rendered-sha256`, so any rendered change
recreates it. To change a setting, change the variable (or the route, by PR) and run Deploy
`upgrade`. [product/render-compose.sh](product/render-compose.sh) renders the product the same way.

### RPC providers

Every route names its chain's RPC providers by id in `chain.rpc_providers`, at least two different
providers, and a route that names none uses `provider-a` and `provider-b` (the committed Sepolia
routes do). An id is lowercase letters, digits, and `-`, and names its two variables, the id
upper-cased with `-` as `_`:

| Variable | What | Where |
|---|---|---|
| `TOPUP_RPC_<ID>_URL` | the provider's HTTPS URL for its chain; a provider that puts its API key in the URL is set with `{key}` in the key's place (`https://eth-mainnet.g.alchemy.com/v2/{key}`, `https://mainnet.infura.io/v3/{key}`, `https://NAME.quiknode.pro/{key}/`) | an [attested setting](#attested-settings): the Environment variable of the same name |
| `TOPUP_RPC_<ID>_KEY` | the key that fills `{key}`; only for a provider that may take one | [sealed](#sealing-the-secrets): a name of [staging.env.example](staging.env.example) |

A provider serves one chain: routes of the same chain name the same providers, and a route of
another chain names providers of its own, even from the same company (`alchemy-base-sepolia` beside
`alchemy-sepolia`). The chain must carry the canonical Multicall3
([contracts/multicall3.json](contracts/multicall3.json)).

The order matters. The first provider, A, makes every `eth_getLogs` ([Measuring RPC
usage](#measuring-rpc-usage)): windows of up to 2 000 blocks, and in address mode and the
reconciler's missing-deposit check, transfers by recipient with no contract address. Some public
endpoints refuse one or the other (a 1 000-block range cap; a required `address`), so check both
before naming one first. Provider B never reads logs, only receipts, heads (`latest`, `safe`,
`finalized`), nonces, and calls.

The `x-rpc-providers` block of [docker-compose.yml](docker-compose.yml) lists each provider once,
its URL and, if it may take a key, its key, and gives them to `topup` and `restore-check`.
[Preflight](preflight.sh) requires every provider a route names there and no other, an `https` URL
that does not embed a key, a key where the URL has `{key}` and none where it has not, each route's
providers at different URLs, and, online, each provider reporting the chain of every route that
names it, with the route's contracts and asset on it.

**Adding a chain** is configuration, in one PR and one Deploy `upgrade`: the route file and its
copy in the compose ([self-hosting, "Routes and contracts"](../docs/self-hosting.md#3-routes-and-contracts)),
with `chain.rpc_providers` naming two new ids; their `TOPUP_RPC_<ID>_URL` lines in
`x-rpc-providers`; for a keyed provider, its `TOPUP_RPC_<ID>_KEY` line there, in
[staging.env.example](staging.env.example), and in [app-compose.example.json](app-compose.example.json)'s
`allowed_envs`; the chain in [check-route-modes.sh](check-route-modes.sh) and
[contracts/networks.json](contracts/networks.json) if they lack it; and, before the upgrade, the
Environment variables `TOPUP_RPC_<ID>_URL` in every Environment and, for a new sealed name, the
[re-seal](#sealing-the-secrets) with it.

### Custom domain

The API has one public origin, `https://$TOPUP_DOMAIN`, a stable name the owner controls, not
the CVM's gateway URL, which changes with the node and the app id: merchants call it and pin its
attestation, treasury challenges (EIP-4361) name it as their `domain` and `uri`, and the admin
API verifies every signed `@target-uri` against it. The official [dstack-ingress](https://github.com/Dstack-TEE/dstack-examples/tree/dstack-ingress-v2.6/custom-domain/dstack-ingress)
2.6 (`ghcr.io/dstack-tee/dstack-ingress`, pinned by the digest of its release notes; `gh
attestation verify oci://ghcr.io/dstack-tee/dstack-ingress@sha256:c212abb7bedec4d7b54a82bcf9972e58e39a4757cc811faeb6017eee6c0673b0
--owner Dstack-TEE` shows it was built from tag `dstack-ingress-v2.6`) publishes the compose's only
port, 443. The gateway passes the TLS connection for the domain through to it, it terminates TLS
inside the CVM, and forwards the stream to `topup:8080`. It gets the Let's Encrypt certificate
with `tls-alpn-01` through that same port, so the CVM holds no DNS credentials; the ACME contact is
unset because the account document is published. Like `keys` and `topup` it mounts the dstack
socket (its instance id and the evidence quote), which is why it is pinned by digest and attested
with the compose.

The staging reference product is served the same way: the same pinned dstack-ingress in its
compose terminates TLS for `$PRODUCT_DOMAIN` (Phala's: `pay-demo-api.phala.com`) and forwards to
`product:8089`, so the demo's API, the product's webhook endpoint, and its account API are at
`https://$PRODUCT_DOMAIN`. Phala's website, `pay.phala.com`, is not a CVM's: Cloudflare serves it
([Website](#website)).

**HUMAN-ONLY, owner of the domain's DNS zone** (any DNS provider; the Deploy summary's
wording assumes Cloudflare, as in Phala's instance), once per CVM instance. Every Deploy run
lists the records for its target's domain (`$DOMAIN`: `$TOPUP_DOMAIN` or `$PRODUCT_DOMAIN`), in
the tls-alpn-01 format of the pinned README:

| Type | Name | Content |
|---|---|---|
| CNAME | `$DOMAIN` | the CVM node's gateway, `gateway.<base domain>` |
| TXT | `_dstack-app-address.$DOMAIN` | `<instance_id>:443` |
| CAA (optional) | `$DOMAIN` | `0 issue "letsencrypt.org;validationmethods=tls-alpn-01;accounturi=<ACME account>"` |

- Not proxied (on Cloudflare, DNS only, grey cloud): a proxied name resolves to the proxy, so
  neither the CA nor a client reaches the gateway.
- The TXT names the instance, not the app: the CA's validation must reach the one instance that
  holds the ACME order. An upgrade keeps the instance id; a new instance ([Resume](RESTORE.md#resume))
  serves the domain only after the TXT carries its id.
- Until both records resolve, dstack-ingress serves a self-signed placeholder and requests no
  certificate, so Deploy's `/healthz` wait on the domain fails.
- CAA is optional. An existing CAA record on the domain or a parent domain must permit
  `tls-alpn-01` (Phala's `phala.com` has none as of 2026-09-26). Pinning `accounturi` to the account that
  [verify-ingress-evidence.sh](verify-ingress-evidence.sh) prints also means that losing the
  `ingress_certs` volume (a new account) blocks renewal until the record is updated.

**Certificate evidence.** dstack-ingress publishes, at `https://$TOPUP_DOMAIN/evidences/`, the
ACME account, the certificate, `sha256sum.txt` over both, and a TDX quote whose `report_data` is
the hash of `sha256sum.txt`. Its evidence server listens on the ingress container's loopback (port
80) and is reached only through 443, where HAProxy routes `GET /evidences` to it, so there is no
evidence port to publish. [verify-ingress-evidence.sh](verify-ingress-evidence.sh) checks the
chain with the official verifier (the quote is app `APP_ID`'s and binds the files) and that the
domain serves exactly that certificate; Deploy runs it after every topup upgrade:

```sh
deploy/verify-ingress-evidence.sh "$TOPUP_DOMAIN" "$APP_ID"
```

The quote dates from the last issuance, so its compose hash can be an earlier compose of the app.

### Database credentials

PostgreSQL runs inside the CVM, so its passwords are derived there, never supplied. `keys`
(`topup keys`) derives the WAL-G key (`get_key("backup/v1")`) and the owner and application
passwords (hex of `get_key("db/owner/v1")`, `get_key("db/app/v1")`) into tmpfs volumes; each
service mounts only what it needs, read-only, and `validate-compose.sh` enforces it:

| Volume | Files | Mounted by |
|---|---|---|
| `walg_key` (`/run/wal-g`) | `backup.key` | `postgres`, `backup`, `restore` |
| `db_owner` (`/run/db-owner`) | `postgres.password`, `postgres.pgpass` | `postgres`, `migrate`, `backup`, `restore-check` |
| `db_app` (`/run/db-app`) | `topup_service.pgpass` | `postgres`, `topup`, `heartbeat` |

The same app id derives the same passwords, so a replacement or restored instance logs in
unchanged. Rotation is an `ALTER ROLE` to a `db/*/v2` value and a new compose.

## Sentry

The service reports to Sentry itself, only while `SENTRY_DSN` is non-empty (a malformed DSN stops
`topup run`). The release is the `TOPUP_IMAGE` digest and the environment `SENTRY_ENVIRONMENT`
(the GitHub Environment's name), both attested.

- **Events**: every `ERROR` line and panic, grouped by message, at most one event per issue every
  10 minutes. An event holds the log line's fields minus `account_id`; no request data, no RPC
  URL, no spans, `send_default_pii` off.
- **Alerts**: lines tagged with an alert name, fingerprinted by the name and its grouping
  tags (route, state, check, chain, scope), with a `runbook` tag. The names and their runbooks are
  in the [runbooks index](runbooks/README.md#alert-and-symptom-index).
- **Crons**: each loop checks in and so creates its monitor:

  | Monitor | Checks in | Margin |
  |---|---|---|
  | `topup-scanner-<chain_id>` | after each head poll (every block time), `error` while the finalized backstop fails | 5 min |
  | `topup-pump-<n>`, `topup-outbox-test`, `topup-outbox-live` | each iteration or poll, every minute | 5 min |
  | `topup-lock-expiry` | after each successful expiry scan, every minute | 5 min |
  | `topup-finality-watch` | after each `finalized` advance's passes, and every minute | 5 min |
  | `topup-reconciler` | `ok` after a complete round or one skipped because nothing newly finalized, `error` after failed checks, every 10 min | 10 min |
  | `topup-backup` | `ok` while the WAL-G success marker is at most 120 s old, else `error`; 3 errors open an issue | 2 min |

- **Uptime**: `/healthz` of each Environment ([One-time setup](#one-time-setup-human-only-repository-owner)).
- **Egress**: `topup` sends HTTPS to the DSN's ingest host.

A restore-check instance runs no loop and reports as `<environment>-restore`, so it never checks
in or raises an alert of the live environment.

## Measuring RPC usage

Every JSON-RPC call the service sends is counted by configured provider id (`provider-a`,
`provider-b`, never a URL), chain id, and method (16 named methods; any other counts as
`other`), from process start. The admin-signed `GET /v1/admin/metrics` returns the counters in
the Prometheus text format; with the `admin` helper from the
[runbooks](runbooks/README.md#environment):

```sh
admin GET /v1/admin/metrics
```

```text
topup_rpc_calls_total{provider="provider-a",chain_id="11155111",method="eth_blockNumber"} 8012
topup_rpc_calls_total{provider="provider-a",chain_id="11155111",method="eth_getLogs"} 7390
topup_rpc_calls_total{provider="provider-b",chain_id="11155111",method="eth_getTransactionReceipt"} 214
topup_rpc_calls_since_seconds 1790500000
```

A day's usage is the difference of two readings a day apart (or a counter over
`now − topup_rpc_calls_since_seconds`, scaled to a day); counters restart at zero with the
process. A production CVM has no collector, so read it on staging, or occasionally on
production, with the admin key.

**Cost formula.** For one provider and one chain, with `n(m)` the calls a day of method `m` and
`p(m)` the provider's price of one call of `m` (compute units, credits, or currency: the
provider's current price list is the input, not this document):

```text
cost per day   = Σ_m n(m) × p(m)
cost per month = 30 × cost per day
```

The cadences of docs/architecture.md §8 predict `n(m)` for provider A with `B` blocks a day
(86 400 / block time; 7 200 on Ethereum), `F` `finalized` advances a day (225 on Ethereum),
`R` reconciliation rounds a day (at most 144, and at most `F`), `P` payments a day, `A` issued
addresses, `U` forwarders holding unswept funds, and `L = 1` in token mode or `⌈A / 1 000⌉` in
address mode:

| Method | Calls a day, provider A | Provider B |
|---|---|---|
| `eth_blockNumber` | `1.1 B` (head polls) `+ P` (credit check) | `P` |
| `eth_getBlockByNumber` | `86 400 / finalized poll interval` (1 440) `+ F` (backstop), `+ B` for a `safe` route | `≤ min(P, F)` (finality passes) |
| `eth_getLogs` | `L × B` (per-block scan) `+ (L + 1) × F` (backstop) `+ ⌈A / 1 000⌉ × R` (missing-deposit check) | 0 |
| `eth_getTransactionReceipt` | `3 P` (detection, credit, finality) | `2 P` (credit, finality) |
| `eth_getTransactionByHash` | `P` (the nonce, at detection) | 0 |
| `eth_getBlockByHash` | `≤ B` blocks with a payment, 0 on nodes returning `blockTimestamp` with logs | 0 |
| `eth_call` | `P` (sanctions) `+ ⌈U / 200⌉ × R` (custody) `+` new addresses `/ 200` per round (derivation) | `P` (sanctions) |

Retries (a lagging provider B is re-checked every 2 s; a failed read backs off) add to these;
compare the prediction with the counters before relying on it.

## Attestation, ingress, and egress

**HUMAN-ONLY, verifier**, before onboarding any account. Deploy already verifies the attested
compose; to re-check a CVM from a checkout of the deployed commit, with the run's rendered compose
and `PHALA_CLOUD_API_KEY` exported:

```sh
npx --yes phala@1.1.22 cvms get "$CVM_ID" --json > cvm.json
npx --yes phala@1.1.22 cvms attestation "$CVM_ID" --json > attestation.json
APP_ID=$(jq -er '.app_id' cvm.json) && GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json)
curl -fsS "https://${APP_ID#0x}-8090.$GATEWAY_DOMAIN/prpc/Info" > info.json
deploy/verify-attestation.sh attestation.json info.json "$APP_ID" docker-compose.ENV.yml
export TOPUP_PUBLIC_ORIGIN="https://$TOPUP_DOMAIN"
deploy/verify-ingress-evidence.sh "$TOPUP_DOMAIN" "$APP_ID"
```

[verify-attestation.sh](verify-attestation.sh) runs the official dstack verifier
([dstack-verifier.sh](dstack-verifier.sh), `dstacktee/dstack-verifier:0.5.9` pinned by digest:
TDX quote and TCB, RTMR3 event-log replay, OS image measurements), requires TCB `UpToDate`, the
app id, and a compose hash whose app-compose holds exactly the rendered compose, then checks the
policy: `allowed_envs` equal to the sealed names and the only published port `dstack-ingress` on
443 (`tls-alpn-01`, forwarding to `topup:8080`, for the domain of `TOPUP_PUBLIC_ORIGIN`), or for
the restore-check variant `topup` on 8081 and no ingress. Never treat a hash from
[render-app-compose.sh](render-app-compose.sh) as the deployed one; the Phala CLI builds the
app-compose itself.

An account's webhook keys come only from the nonce-bound attestation, fetched with a secret key of
that account and mode (merchants run the same check, docs/integration.md §5.3):

```sh
export NONCE="$(openssl rand -hex 32)"
curl -fsS -H "Authorization: Bearer $SECRET_KEY" \
  "$TOPUP_PUBLIC_ORIGIN/v1/attestation?nonce=$NONCE" > public-attestation.json
jq '{quote: null, attestation: .tdx_quote}' public-attestation.json |
  deploy/dstack-verifier.sh > public-verification.json
jq -e --arg app "$(jq -r '.app_id | ltrimstr("0x") | ascii_downcase' cvm.json)" \
  --arg compose "$(jq -j '.compose_file' attestation.json | sha256sum | cut -d' ' -f1)" \
  --arg report_data "$(jq -r '.report_data' public-attestation.json)" '
  .details.tcb_status == "UpToDate" and .details.app_info.app_id == $app
  and .details.app_info.compose_hash == $compose
  and .details.report_data == $report_data + ("0" * 64)' public-verification.json
```

Then check that `report_data` binds the nonce, the account, the mode, and the listed webhook keys
with the Python SDK's `topup_sdk.verify_attestation_binding(response, nonce, expected_account=…,
expected_livemode=…)` (architecture §14 defines the construction; `TopupClient.attestation` runs
it on every fetch).

**Ingress**: the attested compose must publish only `dstack-ingress` on 443; confirm `/openapi.json`
at `TOPUP_PUBLIC_ORIGIN` with a valid certificate, its [certificate
evidence](#custom-domain), and that PostgreSQL and topup's port 8080 are unreachable. The admin
API verifies every signed `@target-uri` against `TOPUP_PUBLIC_ORIGIN`, so a correctly signed
admin request answered `401` usually means the URL differs from it. **Egress** (HUMAN-ONLY, cloud network
authority; dstack has no hostname allow-list): restrict outbound traffic to the two RPC hosts,
the price sources, the object storage host, the Sentry ingest host, DNS, the Phala/dstack
platform endpoints, and public addresses on ports 443 and 80 for webhooks (merchants register
their own endpoints, so their hosts cannot be listed; [webhook egress](#webhook-egress) filters
the addresses), and record the rules.

### Webhook egress

Merchants register their own webhook URLs (`/v1/webhook_endpoints`), so a URL may name any
address, including the CVM's own network or a cloud metadata service. Every delivery therefore
leaves through the `smokescreen` sidecar (`TOPUP_WEBHOOK_PROXY=http://smokescreen:4750`), Stripe's
[smokescreen](https://github.com/stripe/smokescreen) and the only IP filter (design §8): it resolves
the host itself and refuses, with `407` before connecting, every address that is not publicly
routable. The service checks only a URL's scheme and port when it is registered (`https` on 443;
in test mode also `http` on 80), follows no redirect, and times out after 20 s; `run` refuses to
start without the proxy unless its own origin is `http` (local stacks).

- **Binary.** Stripe publishes no image, so the phala-pay [Dockerfile](../Dockerfile) builds
  smokescreen v0.1.0 (commit `609eb8931420453daf5893509be0b25b21bd9edb`) with its vendored
  modules in `golang:1.27-trixie` pinned by digest, reproducibly, and the sidecar runs it from
  `TOPUP_IMAGE`: it is pinned and attested with the service's digest.
- **Policy.** Its defaults refuse loopback, private (`10/8`, `172.16/12`, `192.168/16`,
  `fc00::/7`, which holds AWS's IPv6 metadata `fd00:ec2::254`), link-local (`169.254/16` with the
  metadata address `169.254.169.254`, `fe80::/10`), CGNAT (`100.64/10`, with Alibaba's metadata
  `100.100.100.200`), multicast, unspecified, and IPv6 that embeds IPv4 (NAT64 `64:ff9b::/96`,
  6to4, Teredo); an IPv4-mapped IPv6 address (`::ffff:a.b.c.d`) is checked as the IPv4 address it
  maps. The compose adds `--deny-range` for what those defaults count as global: `0.0.0.0/8`,
  `192.0.0.0/24`, `198.18.0.0/15`, and `240.0.0.0/4`, and repeats `100.64.0.0/10` and
  `169.254.0.0/16`. A `--deny-range` of `::ffff:0:0/96` must never be added: Go matches it against
  every IPv4 address and it would refuse all webhooks. No allow-range, ACL, or
  `--unsafe-allow-private-ranges` is set, and
  [validate-compose.sh](validate-compose.sh) fails if one is.
- **Test.** [tests/smokescreen.sh](tests/smokescreen.sh) runs the sidecar from an image with the
  compose's own command and checks, through plain requests and CONNECT tunnels, that private,
  loopback, metadata, CGNAT, `0/8`, IPv4-mapped, NAT64, and unique-local targets are refused and a
  public address is not; CI runs it on every image build.
- **Local stacks** (`make up`, the sandbox, the CVM rehearsal) deliver directly
  (`TOPUP_WEBHOOK_PROXY=""`), because their receivers listen on private compose addresses.
- A merchant's URL that resolves to a refused address fails like an unreachable one: retried with
  backoff (probed about once an hour), never disabled, and visible to the merchant as the
  endpoint's `pending_deliveries`, `oldest_pending_at`, and `last_attempt`, and as each event's
  `pending_webhooks` (`GET /v1/events?delivery_success=false`); the daily report lists endpoints
  failing longer than `failing_for_hours`.

## Contracts

The forwarder factory is permissionless and deterministic (design D3): the committed build deploys,
through the Arachnid proxy, the factory `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747` and its
implementation `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9` at the same addresses on every chain
(`contracts/local-test-vectors.json`; the build since the per-target gas bounds of #202). A route
names only the factory; `topup run` refuses to start unless the chain holds exactly that build's
code there, so the factory is deployed on a chain before any compose with a route on it.

An operator reuses the factory wherever it is deployed (it is on Sepolia; `verify-deployment.sh`
below checks a chain read-only) and deploys it only on a chain where it is missing.
**HUMAN-ONLY, deployer with a funded throwaway EOA**, once per chain ([CONTRACTS.md](CONTRACTS.md)
has the checks each script makes; `$SEPOLIA_RPC_A` and `$SEPOLIA_RPC_B` are two providers):

```sh
read -rsp "Deployer private key: " PRIVATE_KEY && printf '\n' && export PRIVATE_KEY
deploy/contracts/deploy-proxy.sh --rpc-url "$SEPOLIA_RPC_A"          # the Arachnid proxy exists
deploy/contracts/deploy-factory.sh --rpc sepolia/a="$SEPOLIA_RPC_A" --dry-run   # prints both addresses
deploy/contracts/deploy-factory.sh --rpc sepolia/a="$SEPOLIA_RPC_A" --broadcast
unset PRIVATE_KEY
deploy/contracts/verify-deployment.sh --rpc sepolia/a="$SEPOLIA_RPC_A" \
  --rpc sepolia/b="$SEPOLIA_RPC_B" > sepolia-contract-verification.json
jq -e '.passed == true' sepolia-contract-verification.json
```

The dry run must print the two addresses above; if the factory already has that code (anyone may
deploy it), the broadcast sends nothing. Then run **Verify contracts** (Actions), which re-checks the
deployment, the Safe of `contracts/safe-expectations.json` (Phala's), and the committed routes daily,
in the `staging` Environment. Mainnet repeats this after the security review
([CONTRACTS.md, "Mainnet"](CONTRACTS.md#mainnet)).

## Operator onboarding

<a id="account-credentials"></a>
Accounts are created only by the operator, after due diligence done offline (design D8); there is
no signup, dashboard, or email. The operator creates the account, decides live access, sets
platform limits, and issues first or recovery keys; it never sets a treasury, registers a
merchant's endpoint, moves funds, records refunds, or resends a merchant's events. Every admin call
is RFC 9421-signed with the admin key and audited, and each change to an account is announced to it
as an event (`account.updated`, `api_key.created`). With the [runbooks' `admin`
helper](runbooks/README.md#environment), in order:

1. **Due diligence, offline** (HUMAN-ONLY, operator): the business, its owners, sanctions screening
   of the entity, owners, and intended treasuries, jurisdiction, and the signed merchant agreement,
   under Phala's policy (design §17). Until the legal review signs off, live mode is for Phala's own
   accounts only. The product stores only a reference, the date, and the reviewer.
2. **Create** (HUMAN-ONLY, admin key holder, after [attestation](#attestation-ingress-and-egress)).
   The answer holds the account id `acct_…` and, in `api_keys`, its first secret test key
   (`ppay_sk_test_…`) and, with `charges_enabled`, live key; each `secret` is shown only here:

   ```sh
   (umask 077 && admin POST /v1/admin/accounts "$(jq -cn '{name: "<merchant name>",
       contact: {name: "<name>", email: "<security email>"},
       due_diligence: {reference: "<review reference>", reviewed_at: "<YYYY-MM-DD>",
                       reviewed_by: "<reviewer>"},
       charges_enabled: false, reason: "<why>"}')" > ~/onboarding/account.json)
   ```

   The admin seed converts to the PEM the helper signs with once:
   `(umask 077 && { printf '302e020100300506032b657004220420'; tr -d '\n' < admin.seed; } | xxd -r -p | openssl pkey -inform DER -out admin.pem)`.
3. **Key hand-over** (HUMAN-ONLY): send the account id and key to the recorded contact through an
   encrypted channel, then delete `account.json`. The merchant rolls the key at once
   (`POST /v1/api_keys/{id}/roll`), keeps secret keys offline for administration, and runs its
   servers with a restricted key (`POST /v1/api_keys {"type": "restricted", "permissions": […]}`,
   `ppay_rk_…`), which cannot manage keys, treasuries, endpoints, webhook keys, or account settings
   (docs/integration.md §5.4). Everything else the merchant does itself through the API or SDKs:
   [treasury](#treasury-setup), [webhook endpoint](#webhook-endpoint) and webhook key pinning,
   confirmation policy, `quotes` pause, refunds, and [sweeps](#sweeping).
4. **Live enablement** (HUMAN-ONLY, admin key holder), when the review allows it: the answer holds
   the first live key, handed over as in step 3. Live payments also need a proven live treasury.

   ```sh
   admin POST "/v1/admin/accounts/$ACCOUNT" '{"charges_enabled": true, "reason": "<review reference>"}'
   ```

5. **Exposure cap**: `max_unfinalized_credit` caps, in cents and per mode, the credit of the
   account's deposits credited before they are final (default 100 000, $1 000; design, ledger
   correctness amendment); a deposit past it is credited at finality instead, and `0` credits every
   deposit at finality. Raise it on the merchant's request once its volume and its ability to claw
   back justify it:

   ```sh
   admin POST "/v1/admin/accounts/$ACCOUNT" '{"max_unfinalized_credit": 500000, "reason": "<why>"}'
   ```

A merchant that lost every key or cannot win a leak by rolling asks for a recovery key, verified
with the recorded contact ([API key compromise and key recovery](runbooks/api-key-compromise.md)).

## Merchant setup

These are the merchant's steps, done with its own secret key against the operator's
`https://$TOPUP_DOMAIN`. An operator that is also a merchant of its own instance does them as the
merchant, never with the admin key (Phala's staff, for Phala Cloud and the staging reference
product).
The [integration guide](../docs/integration.md) is the full reference.

### Treasury setup

Each account proves a treasury per chain and mode with a signed EIP-4361 challenge (design D10,
docs/integration.md §1.6); quotes and deposit addresses answer `400 treasury_not_set` before. A
chain's first treasury and every test-mode change apply at once; a later live change is `pending`
for 48 hours and cancellable, and is announced as `treasury.created` to every enabled endpoint.

- **EOA (Sign-In with Ethereum).** `POST /v1/treasuries/challenge {chain_id, address}`, sign the
  returned `message` with EIP-191 `personal_sign`, and `POST /v1/treasuries {chain_id, message,
  signature}` within 10 minutes: `pay.treasuries.set_eoa(chain_id=…, address=…, private_key=…)`
  (Python, `phala-pay[eoa]`) or [sandbox/set-treasury.sh](sandbox/set-treasury.sh) with `cast` do
  both.
- **Safe (EIP-1271, via Safe{Core}).** The Safe must be deployed on the chain, and its fallback
  handler must be the `CompatibilityFallbackHandler`, which answers `isValidSignature` (v1.4.1:
  `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99`, [safe-deployments](https://github.com/safe-global/safe-deployments/blob/main/src/assets/v1.4.1/compatibility_fallback_handler.json)).
  Request the challenge for the Safe's address; the owners sign `message` as a **Safe message**
  (Protocol Kit `createMessage` and `signMessage`, or API Kit `addMessage` and
  `addMessageSignature` when owners sign apart), or approve it on chain with `SignMessageLib` and
  submit `"signature": "0x"` once that transaction is final; submit within 24 hours. The exact code
  is in docs/integration.md §1.6. The service checks `isValidSignature` at `finalized` on both
  providers.
- Update the treasury pinned in the merchant's server (the SDKs' `treasuries` pin) when a change
  applies (`treasury.updated`); addresses issued before keep paying the old treasury.

### Webhook endpoint

With a secret key, `POST /v1/webhook_endpoints {"url": "https://…/webhooks", "enabled_events":
["*"]}` (at most 16 per account and mode; `https` on 443 in live mode), then
`POST /v1/webhook_endpoints/{id}/test`. Account events (keys, endpoints, treasuries,
`account.updated`) reach every enabled endpoint whatever it subscribes to. Deliveries are signed
with the account's own key of the mode (Standard Webhooks `v1a`): the merchant fetches
`GET /v1/attestation?nonce=…` with its key, verifies it ([Attestation](#attestation-ingress-and-egress);
docs/integration.md §5.3), and pins the public keys; the SDKs' `construct_event` then refuses any
event not signed by them for its account and mode. Retries never stop; the endpoint's
`pending_deliveries`, `oldest_pending_at`, and `last_attempt` show its health, and
`POST /v1/events/{id}/resend` resends.

### Sweeping

The service sends no transactions and holds no key that can move funds (design D4). Payments wait in
forwarders, each able to pay only the treasury its address commits to, until the merchant sweeps
them with one `factory.flush(treasury, salts, token)` per token and treasury, from its own wallet or
Safe, paying the gas. The SDKs build the call offline from the forwarder export:

```python
from topup_sdk import flush_transactions, safe_batch, write_safe_batch

forwarders = list(pay.forwarders.list(chain_id=11155111, sweepable=TOKEN))  # final, never sanctioned
calls = flush_transactions(forwarders, TOKEN)   # {to, data, value}, one per treasury, 200 salts each
# EOA treasury or any wallet: send each call as an ordinary transaction.
# Safe treasury: a Safe{Wallet} Transaction Builder batch for the owners to import, sign, execute.
write_safe_batch("sweep.json", safe_batch(11155111, SAFE, calls, name="Phala Pay sweep"))
```

`GET /v1/balance` reports what forwarders hold per chain and token (and the final part), and
`GET /v1/sweeps` every finalized `Flushed` event, whoever sent it; deposits are marked `swept` about
15 minutes after the sweep, at finality. A target whose transfer failed (`FlushFailed`, a token or
treasury refusing it) stays unswept for the merchant to resolve. A token that needs more than the
factory's 200 000 gas per transfer cannot be swept and is never enabled in a route.

## After a restore: the merchant notice

A restored service starts frozen in restore mode (design §13): reads work, every merchant write
answers `503 service_restoring` with `Retry-After`, and nothing credits or delivers until the
operator's reconciliation and unfreeze ([Reconciliation after a restore](runbooks/restore.md)).
**HUMAN-ONLY, operator**: before reconciling, send every account's recorded contact the notice:
the restore point (the backup time) and the time the restore was detected; that writes answer `503`
until further notice, so retry with the same `Idempotency-Key`; and the records the operator needs
from the merchant, received after the restore point: key revocations (id, or prefix and last four),
treasury cancellations and crediting pauses, deleted endpoints, deposit addresses given to
customers, and delivered deposit events. After the unfreeze, send a second notice: service resumed,
any re-valued deposit flagged, and events after the restore point delivered again. The runbook
[Incident communication](runbooks/incident-communication.md) has the channels.

## Local verification

- `make up` / `make down`: the attested compose rendered with local settings plus the
  [local overlay](local/docker-compose.yml) (Garage S3, the dstack simulator, a mock product);
  run manual commands through `deploy/local/compose.sh`.
- `make cvm-rehearsal`: the staging artifact itself against Anvil, Garage, and the simulator,
  from the unsealed boot through sealing, operator onboarding of the product's account, its
  treasury proof and webhook endpoint, and one credited deposit.
- `make restore-drill`: [RESTORE.md](RESTORE.md#local-and-ci-drills).
- `make sandbox-local`: the integrator sandbox ([sandbox/README.md](sandbox/README.md)).
- `deploy/validate-compose.sh`: the compose policy CI enforces.

## Phala's instance

Phala runs an instance only for Phala Cloud and offers no hosted service to others. Its
`staging` Environment (`https://pay-api-staging.phala.com`, Sepolia) also runs a reference
product whose API serves the live demo on Phala's website, [pay.phala.com](https://pay.phala.com/).
This section records that setup; another operator needs none of it, and can run the reference
product the same way for its own rehearsals.

### Staging routes

Staging serves four test-mode routes, two on Sepolia and two on Base Sepolia, all on the
deterministic factory ([Contracts](#contracts)); any test key quotes on all of them, and
`GET /v1/config` lists each chain's assets:

| Route | Token | Pricing | Test tokens |
|---|---|---|---|
| `phala-cloud-sepolia-pha-usd` ([file](config/routes/phala-cloud-sepolia-pha.yaml)) | test PHA `0x8F40e7E99678F44c88158f049E62817580ab113B` (`MockERC20`, 18 decimals) | spot: Coin Metrics `pha`, checked against Binance `PHAUSDT` | `mint(address,uint256)` is public |
| `phala-cloud-sepolia-usdc-usd` ([file](config/routes/phala-cloud-sepolia-usdc.yaml)) | Circle's testnet USDC `0x1c7D4B196Cb0C7B01d743Fbc6116a902379C7238` ([Circle's list](https://developers.circle.com/stablecoins/usdc-contract-addresses), 6 decimals) | stablecoin: 1.00 while Coin Metrics' `usdc` rate is within 1% | [Circle's faucet](https://faucet.circle.com) (Ethereum Sepolia) |
| `phala-cloud-base-sepolia-pha-usd` ([file](config/routes/phala-cloud-base-sepolia-pha.yaml)) | test PHA `0x1a6F260377e42ead1418C7C1afDFD5DE371A9284` (the same `MockERC20`, 18 decimals) | as on Sepolia | `mint(address,uint256)` is public |
| `phala-cloud-base-sepolia-usdc-usd` ([file](config/routes/phala-cloud-base-sepolia-usdc.yaml)) | Circle's testnet USDC `0x036CbD53842c5426634e7929541eC2318f3dCF7e` ([Circle's list](https://developers.circle.com/stablecoins/usdc-contract-addresses), 6 decimals) | as on Sepolia | [Circle's faucet](https://faucet.circle.com) (Base Sepolia) |

| Chain | `confirmations` | RPC providers | Sanctions oracle (a `MockSanctionsOracle`) |
|---|---|---|---|
| Sepolia (11155111) | `2`, Ethereum L1's default | `provider-a`, `provider-b` | `0x28A73f8235d966244210D9c49E34EDdA4fF9e1f6` |
| Base Sepolia (84532) | `safe`, the OP-stack default: about 5 minutes, never the sequencer's unsafe head (architecture §8) | `base-sepolia-a` `https://base-sepolia.gateway.tenderly.co`, `base-sepolia-b` `https://base-sepolia-rpc.publicnode.com`, both keyless | `0x8A0C93d85a05aD30741C193068abF2e5E16e7b35` |

There is no USDT route: Tether publishes no testnet USDT, and a third-party token is not one.
USDC moves one or two transfers a block on both chains, so its routes set `backstop: addresses`,
which puts each whole chain, PHA included, on transfer requests by recipient (architecture §8); the
RPC cost is unchanged while staging has fewer than 1 000 addresses ([Measuring RPC
usage](#measuring-rpc-usage)). Base's public `https://sepolia.base.org` is not a provider: it
caps `eth_getLogs` at 1 000 blocks. publicnode refuses logs with no contract address, so it is
provider B ([RPC providers](#rpc-providers)). The head loop polls every 12 s on both chains
(`--head-poll-interval-s`), six Base blocks, so Base Sepolia costs about what Sepolia does.
Routes are attested config: adding or changing one is a PR and a Deploy `upgrade` of `topup`,
never a reset.

### Staging reset (HUMAN-ONLY)

The multi-tenant schema (design §14) replaced the migration history and migrates no data
(`crates/topup/migrations/README.md`), and staging's route now uses the new factory, so the staging
service is replaced, not upgraded: a new CVM on an empty backup prefix, with every account created
again. Nothing on staging is live, so no funds or merchants are affected. Every step below is
HUMAN-ONLY except the workflow runs, which the staging owner dispatches; agents and CI run none of
them. In order:

1. **Merge and build.** This change is on `main`; run Release images and note its run id.
2. **Factory on Sepolia: done.** The factory `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747` and its
   implementation `0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9` are deployed and verified
   ([Contracts](#contracts)); confirm Verify contracts is green.
3. **Treasury Safe: done.** The staging finance Safe `0x936c1991f8dA9a919fa11b557a3514719f5A4504`
   (v1.4.1, 1-of-1) has the `CompatibilityFallbackHandler` v1.4.1
   `0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99` as its fallback handler (Sepolia transaction
   `0xc63baf595bd13f9f27c27ba2a370c602bb2008c8703ab9629095af8844f10812`), so it can prove itself as
   a treasury; [contracts/safe-expectations.json](contracts/safe-expectations.json) records it, and
   `deploy/contracts/verify-safe.sh` passes on both providers. The reference product's account has
   since moved to the staging Safe `0x26430107887d4a691B340BdB887096B83E7a5844`, the same address
   (SafeL2 v1.4.1, 1-of-1, the same fallback handler) on Sepolia and Base Sepolia. Never use
   `0x936c…4504` on Base Sepolia: a copy exists there whose owner key is destroyed.
4. **Stop the old service.** `npx --yes phala@1.1.22 cvms stop "$TOPUP_CVM_ID"` and the same for
   `$STAGING_PRODUCT_CVM_ID`. Keep both CVMs and the old backup prefix for the retention period: they
   restore only with an image built before the reset. Record their ids.
5. **Point staging at an empty database.** Set the `staging` variable `WALG_S3_PREFIX` to a new,
   empty prefix (PostgreSQL initializes a cluster only on a prefix that provably holds no backup,
   [RESTORE.md](RESTORE.md#bootstrap-from-backup)); clear `TOPUP_CVM_ID` and
   `STAGING_PRODUCT_CVM_ID`.
6. **Provision the service.** Deploy (`staging`, `topup`, `provision`, the release of step 1); set
   `TOPUP_CVM_ID` to the new id; [seal the secrets](#sealing-the-secrets); update the
   [DNS records](#custom-domain) the summary lists (the CNAME to the new gateway, the
   `_dstack-app-address` TXT to the new instance); then Deploy `upgrade` with the same release,
   which waits for `/healthz` and verifies the attestation and the certificate evidence. The new
   app id derives new webhook keys for every account.
7. **Verify** the attestation from your machine ([Attestation](#attestation-ingress-and-egress)) and
   that `GET /v1/config` with any test key lists the Sepolia assets with `confirmations` 2 (the
   routes' versions are in the attested compose Deploy `upgrade` verified).
8. **Onboard the staging accounts** ([Operator onboarding](#operator-onboarding), steps 1–3, with
   `charges_enabled: false`: Sepolia routes are test routes), first the reference product's, then
   each internal merchant's (Phala Cloud's staging backend), and send each contact its `acct_…` and
   key.
9. **Set up the reference product's account** as its merchant ([Staging reference
   product](#staging-reference-product), steps 2–4): roll the key, a restricted key for the product,
   the treasury (step 3's Safe, as a Safe message), and, after the product is provisioned, its
   webhook endpoint.
10. **Run one deposit** of each collection method ([Staging reference product](#staging-reference-product),
    step 5) and one [sweep](#sweeping) from the treasury Safe; confirm `swept` and the daily report.
11. **Retire the old CVMs** once the new service has run clean for a day: `npx --yes phala@1.1.22
    cvms delete "$OLD_CVM_ID" --force` for each, by the recorded id (never by name or app id); delete the old backup prefix only at the end of its retention.

### Staging reference product

Staging's reference product is a merchant like any other, with its own account, and a second CVM
running [product/reference_product](product/reference_product): `serve` mode is the webhook
receiver that applies every `deposit.*` snapshot by the balance rule (a deposit nets to
`amount − amount_refunded − amount_reversed` while `credited` or `reversed`; its tests are in
`product/tests`), an account API, and the API of the website's live demo, with a SQLite ledger;
`deposit` mode, run
from an operator's machine, plays a customer and signs the product's account API with a separate
driver key (`driver/v1`, the product's own authentication, not Phala Pay's).

- **Its key.** The sealed env holds only `PRODUCT_API_KEY`, the account's **restricted** test key
  (`ppay_rk_test_…`) with exactly the permissions it uses, listed in
  [product/staging.env.example](product/staging.env.example): `account.read`, `quotes.write`,
  `deposit_addresses.write`, `deposits.read`, `refunds.write`, `sweeps.read`, `forwarders.read`.
  Its preflight ([product/preflight.sh](product/preflight.sh)) accepts only a test key, restricted
  or secret. The account's secret key stays with the staging owner, offline.
- **Its pins.** The attested product config names its `account` (`acct_…`), the forwarder factory
  and implementation (the same on every chain), and, for each of its `chains` (Sepolia and Base
  Sepolia), the account's treasury there, from which the SDK recomputes every quote and
  deposit address before the product shows it; the placeholder `acct_000…` fails the online
  preflight until a PR sets the real id. It pins its account's test-mode webhook keys from the
  authenticated attestation at `TOPUP_ORIGIN`, fetched with `PRODUCT_API_KEY`, at startup or on the
  first webhook when the key is sealed later (until then it answers `503`, and topup retries).
- **Attested settings.** `TOPUP_ORIGIN` (`https://$TOPUP_DOMAIN`), `PRODUCT_PUBLIC_URL`
  (`https://$PRODUCT_DOMAIN`, its [custom domain](#custom-domain)), `PRODUCT_DOMAIN` and
  `PRODUCT_GATEWAY_DOMAIN` (dstack-ingress's), and `PRODUCT_DRIVER_PUBLIC_KEY`. The config itself
  commits each chain's `rpc_url`, a keyless public RPC (publicnode's; the product seals no RPC key,
  and its preflight refuses a keyed URL and checks online that each reports its chain and that the
  chain's treasury is a contract), its test tokens, `bonus_bps` (the demo merchant's own +10% on
  credits paid in PHA, a promotion, not a Phala Pay feature), and `web_origin`,
  `https://pay.phala.com`, the only origin the demo's API allows.
- **Its networks.** The page offers a configured chain only once the service serves assets there
  (`GET /v1/config`): Base Sepolia appears when its route is deployed, with no product change.

The product serves the JSON API of the live demo on the public **Phala Pay website**
([pay.phala.com](https://pay.phala.com/), [product/web](product/web), served by Cloudflare:
[Website](#website)) at `PRODUCT_PUBLIC_URL/api/`
([reference_product/demo.py](product/reference_product/demo.py)); it serves no page. The page
calls it cross-origin: the API answers CORS preflights and sends
`Access-Control-Allow-Origin: https://pay.phala.com` with `Access-Control-Allow-Credentials: true`
and `Vary: Origin` on every `/api/` response, errors included, and nothing of CORS to any other
origin; `/webhooks`, `/healthz`, and `/accounts` have no CORS. The page is a short headline, the
live demo, and the key properties. The demo
sets the product beside its backend (stacked on narrow screens): first, what the customer sees (a cloud console's
billing page, framed as the merchant's app); then what the merchant's backend sees (the
payment's live event stream, then tabs for payments, refunds, sweeps, API requests and webhooks,
and the attestation). The billing page has both ways to collect a payment: a **quote** (a locked price and an exact amount, paid through
`@phala/pay`'s `<Checkout expectedAddress>`) and the visitor's single **deposit address** (every
token on every network, any amount credited at spot, its payments read by the browser with the
address's `client_secret`). An order id set as `metadata` arrives in the `deposit.credited` event.
A timeline built only from real data (chain block times, the service's objects read with the
product key, this product's verified webhooks and ledger rows) shows each payment received, credited
(at two confirmations), final, and reversed if it is; the ledger follows the balance rule. Refunds
follow the merchant flow: declare (a final deposit), pay from the treasury of the deposit's
address, `mark_paid`, verified at finality; on staging that treasury is the finance Safe, so a
visitor who pays from their own wallet sees the verification fail (`sender_mismatch`). Sweeps are
the merchant's: the page shows the unswept balance, the `flush` call and the Safe Transaction
Builder batch the SDK builds, and the finalized sweeps; the product holds no wallet key. Each
browser gets a random demo account in a cookie of the API's origin (`HttpOnly; Secure;
SameSite=Lax; Path=/`, host-only: the two origins are same-site under `phala.com`, so the page's
credentialed requests carry it); quote creation is rate-limited per account (3 a minute, 20 a day)
and overall (30 a minute), POSTs must be JSON, and the page carries a strict CSP. Test PHA is
minted by the visitor's own wallet on the selected network (`mint` is public on the staging tokens);
test USDC comes from Circle's faucet, and gas from each testnet's public faucets. With `sdk/js` built, `cd product/web && pnpm run e2e` runs the whole flow on
Anvil, with the real factory at its deterministic address, against a stand-in service
([product/web/e2e/fake_service.py](product/web/e2e/fake_service.py)): it builds the page against
the local product and serves it from its own origin under the CSP of `public/_headers`, so the
demo runs cross-origin, with CORS and the cookie, as in production.

Setup, in order, after the [staging reset](#staging-reset-human-only)'s steps 1–8 (each step
**HUMAN-ONLY** unless it is a workflow run):

1. On the owner's machine (mode-0600 files, never committed), create the driver key and set the
   `staging` variable `PRODUCT_DRIVER_PUBLIC_KEY` (the driver's printed `public_key`):

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid driver/v1 --seed-out ~/staging/driver.seed
   ```

2. **As the product's merchant**, with the account's first secret test key from
   [onboarding](#operator-onboarding): roll it, then create the product's restricted key and keep
   its `secret` for step 4:

   ```sh
   curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/api_keys" -H "Authorization: Bearer $SECRET_KEY" \
     -H 'content-type: application/json' -d '{"name": "reference product", "type": "restricted",
     "permissions": ["account.read", "quotes.write", "deposit_addresses.write", "deposits.read",
     "refunds.write", "sweeps.read", "forwarders.read"]}'
   ```

3. **Treasury Safe owners**: set the account's treasury on each of its chains to the staging Safe
   ([Treasury setup](#treasury-setup), Safe message; in test mode it applies at once). Open a PR
   setting the product config's `account` in [product/docker-compose.yml](product/docker-compose.yml)
   to the new `acct_…` id, and merge it.
4. Deploy (`staging`, target `product`, `provision`), set `STAGING_PRODUCT_CVM_ID`, create the
   [DNS records](#custom-domain) for `$PRODUCT_DOMAIN` the summary lists, seal `.env.product`
   holding `PRODUCT_API_KEY=<ppay_rk_test_…>` with the two commands it prints, and Deploy
   `upgrade` with the same release, which waits for `https://$PRODUCT_DOMAIN/healthz` and verifies
   the certificate evidence. Then, with the secret key, register the product's endpoint:
   `POST /v1/webhook_endpoints {"url": "<PRODUCT_PUBLIC_URL>/webhooks", "enabled_events": ["*"]}`
   and `POST /v1/webhook_endpoints/{id}/test`.
5. Run a deposit. The payer is a Foundry keystore with a throwaway key and some testnet ETH; the
   test PHA token is a `MockERC20` with a public `mint`, so the driver mints the quoted amount and
   pays it. `driver.json` holds the `ProductConfig` fields: `service_url` (topup's origin),
   `account`, `factory`, `implementation`, `chains` (as in the product config: each with its
   `chain_id`, `name`, `rpc_url`, `treasury`, and `test_tokens`), and `public_url` (the product
   URL). The driver pays on the first chain, with its first test token; `--chain-id 84532` pays on
   Base Sepolia instead, once its route is served.

   ```sh
   export ETH_KEYSTORE=~/.foundry/keystores/staging-payer ETH_PASSWORD=~/staging/payer.password
   PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product deposit \
     --config driver.json --driver-seed-file ~/staging/driver.seed \
     --amount-minor <cents>
   ```

   The driver recomputes the quote's address before paying and exits 0 once the product has
   recorded exactly one credit and the verified `deposit.credited` webhook (about 30 seconds after
   paying, at the route's two confirmations). `--min-atomic` refuses a quote below that many atomic
   units and prints the `--amount-minor` needed; the quote must also fit the 500000-cent
   per-deposit and per-account caps (PHA below about $0.24). `--until swept` also waits until the
   merchant [sweeps](#sweeping) the forwarder and the sweep is finalized. The deposit address is
   exercised from the demo page: pay any amount of test PHA to it.

`make cvm-rehearsal` runs this product CVM locally, with one deposit.

#### Abnormal paths

The driver also plays the sandbox scenarios' abnormal payments against staging, each in a fresh
workspace, and checks the deposit state, the verified webhooks, and the product ledger
(architecture §7, §9, §15):

| Path | Options | Expected |
|---|---|---|
| underpayment | `--pay-bps 9700` | `credited` at spot for what arrived, then `swept`; the lock later expires |
| after the quote window | `--pay-after-expiry` | `quote.expired`, then `credited` at spot and `swept` |
| unsupported token | `--token T --until rejected` | once final, at the next reconciliation round, `rejected(unsupported_asset)`; the tokens stay in the forwarder; `TopupUnsupportedInflows` |
| refund | `--pay-bps N --until refunded --refund-to A` | a payment of N/10000 of the quote above `max_deposit_atomic` (200000 test PHA): `rejected(out_of_bounds)`, swept; once the deposit is final the driver requests a refund and waits while the treasury Safe's owners, as the merchant, pay it from the treasury of the deposit's address and attaches the transaction with `POST /v1/refunds/{id}/mark_paid`, until `succeeded` and one `deposit.refunded` |

Each row adds its options to the step-5 driver command: `T` is the Sepolia unsupported test token
`0x287E3577c66866a3F5Cb7a8Dac6761EB43608392`, `A` an address the staging owner controls, and the refund
row needs `--timeout 43200`. A mismatch between the expected and the observed outcome exits
non-zero with the reason.

### Website

`pay.phala.com` is the static build of [product/web](product/web), served by the Cloudflare Worker
`phala-pay-web` with static assets only (no Worker script) and deployed by **Cloudflare Workers
Builds**, connected to this repository. Its dashboard settings: root directory
`deploy/product/web` and build command `npm run build:cloudflare`; the deploy commands are the
defaults, `npx wrangler deploy` on `main` and `npx wrangler versions upload` on other branches.
Workers Builds uses the wrangler pinned in [product/web/package.json](product/web/package.json) and
ignores a `build` section of the Wrangler config.

- **Build.** `build:cloudflare` builds `sdk/js` (the page depends on it through `file:`) and then
  the page, each from its own lockfile with `npx -y pnpm@12.6.0`, on the Node of
  `product/web/.node-version` (24, as CI). The page's API origin is fixed at build time:
  `VITE_DEMO_API_ORIGIN` in `product/web/.env.production`, `https://pay-demo-api.phala.com`.
- **[wrangler.jsonc](product/web/wrangler.jsonc).** The assets of `./dist`; any path but the page
  and its assets is a real `404` (`not_found_handling: "none"`); the custom domain `pay.phala.com`
  as a `custom_domain` route; no `workers.dev` copy of the site (`workers_dev: false`); and preview
  URLs on (`preview_urls: true`) for the versions branch builds upload.
- **[public/_headers](product/web/public/_headers).** Cloudflare's static-assets headers: the
  page's CSP (`connect-src` names only the demo API and `pay-api-staging.phala.com`, whose public
  quote and deposit address views the SDK components read), `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `no-cache` for the page, and a year's immutable caching for the
  content-hashed `/assets/*`.
- **Pull requests.** Each branch build uploads a preview version with its own `workers.dev` URL,
  to review the page. Its demo API calls are refused by CORS by design: the API allows only
  `https://pay.phala.com`.
