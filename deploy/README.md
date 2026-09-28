# dstack deployment

Every CVM is deployed by GitHub Actions from `main`; nothing is deployed from a laptop. Steps
outside the workflows that change a registry, Phala Cloud, a CVM, a Safe, a contract, or a secret
are marked **HUMAN-ONLY**. Backup and restore: [RESTORE.md](RESTORE.md). Incident response:
[runbooks](runbooks/README.md). Contracts: [CONTRACTS.md](CONTRACTS.md).

## What runs where

| Where | What | Deployed by |
|---|---|---|
| topup CVM, one per Environment (`staging`, `production`) | [docker-compose.yml](docker-compose.yml): `keys` (derives the database passwords and the backup key), `postgres` (PostgreSQL 18 + WAL-G), `migrate`, `topup` (the service, or read-only and published on 8081 in the [restore-check variant](RESTORE.md#the-restore-check-variant)), `smokescreen` (the [webhook egress](#webhook-egress) proxy), `dstack-ingress` (the only public port, 443: TLS for the [custom domain](#custom-domain), service variant only), `heartbeat`, `backup`, `restore-check` (acts only in that variant) | Deploy, target `topup` |
| Staging reference-product CVM | [product/docker-compose.yml](product/docker-compose.yml), port 8089 | Deploy, target `product` |
| Object storage (Cloudflare R2) | encrypted WAL-G base backups and WAL under `WALG_S3_PREFIX` | owner |
| Sentry project `phala-network/crypto-topup-service` | errors, alerts, Crons and Uptime monitors | the service itself |
| Ethereum (Sepolia for staging) | the permissionless forwarder factory, at one deterministic address on every chain; forwarders; each account's own treasury | factory: any deployer ([CONTRACTS.md](CONTRACTS.md)); treasuries: each merchant, through the API |
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

1. **Environments.** Repository Settings > Environments: `staging` and `production`, deployment
   branches `main` only, no required reviewers (not available on this plan). Whoever dispatches
   Deploy is accountable; the run's actor, summary, and uploaded record are the audit trail.
2. **Phala Cloud.** Create an API key for the Environment's workspace and store it as the
   Environment secret `PHALA_CLOUD_API_KEY`, the only secret GitHub holds.
3. **Object storage.** Create an R2 bucket (or prefix) per Environment that the other
   Environment's keys cannot reach, and a read-write API token for it. The token is sealed into
   the CVM (below), never stored in GitHub.
4. **Environment variables**, per Environment:

   | Name | Value |
   |---|---|
   | `PHALA_WORKSPACE` | display name of the API key's workspace (preflight checks it) |
   | `TOPUP_CVM_ID` | empty until the first provisioning, then the CVM id from the run summary |
   | `TOPUP_DOMAIN` | the [custom domain](#custom-domain): `pay-api-staging.phala.com` (`staging`), `pay-api.phala.com` (`production`) |
   | `AWS_ENDPOINT` | `https://<account>.r2.cloudflarestorage.com` |
   | `WALG_S3_PREFIX` | `s3://BUCKET/PATH`; a new app needs a prefix of its own ([RESTORE.md](RESTORE.md#bootstrap-from-backup)) |
   | `TOPUP_ADMIN_PUBLIC_KEY` | from `topup-sdk keygen --keyid admin/<Environment>-v1`, a separate key per Environment; the seed stays with the admin |
   | `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | HTTPS RPC URLs of the route's chain from two different providers; they are published in the compose, so a provider that puts its API key in the URL is set with `{key}` in the key's place (`https://eth-mainnet.g.alchemy.com/v2/{key}`, `https://mainnet.infura.io/v3/{key}`, `https://NAME.quiknode.pro/{key}/`) and the key is sealed as `TOPUP_RPC_PROVIDER_A_KEY`/`_B_KEY` ([Sealing the secrets](#sealing-the-secrets)); preflight refuses a URL that embeds a key. The chain must carry the canonical Multicall3 ([contracts/multicall3.json](contracts/multicall3.json)) |
   | `STAGING_PRODUCT_CVM_ID`, `PRODUCT_DRIVER_PUBLIC_KEY` | `staging` only: [Staging reference product](#staging-reference-product) |

   No variable or secret names a treasury or a transaction-signing key: treasuries are each
   account's own, set through the API, and the service sends no transactions.

   That is ten variables for `staging` and eight for `production`. All but the first two are
   [attested settings](#attested-settings). Deploy derives the rest, and a variable of the same
   name overrides a derived value where noted:

   | Setting | Derived as |
   |---|---|
   | `SENTRY_ENVIRONMENT` | the Environment's name (no override) |
   | `TOPUP_GATEWAY_DOMAIN` | `gateway.<base domain>` of the CVM's node, read from the existing CVM on `upgrade`; `provision` renders a provisional value and upgrades the new CVM once its node is known (no override) |
   | OS image | `dstack-0.5.9`, fixed in `deploy.yml` (architecture §14; no override) |
   | `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE` | `auto` and `true` for an R2 `AWS_ENDPOINT`; set both variables for any other endpoint |
   | `TOPUP_ADMIN_KID` | `admin/<Environment>-v1`; set the variable only after an admin key rotation to a new key id |
   | `PRODUCT_RPC_URL` | `TOPUP_RPC_PROVIDER_B_URL`, which must then be keyless (product preflight refuses a keyed URL) |

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
   `phala-pay-reference-product` public (organization Packages > package > Package settings >
   Change visibility; the organization must allow public container packages). This is
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

**Production** additionally needs the factory deployed on mainnet at its deterministic address
([CONTRACTS.md](CONTRACTS.md#mainnet), HUMAN-ONLY) and a reviewed route PR putting the mainnet
route into the compose (Deploy refuses a `production` compose with any route off chain 1). After
the first deploy, in order: seal the secrets; [verify the attestation](#attestation-ingress-and-egress);
have Finance, Risk, and Operations approve the pilot limits (route bounds and exposure caps, each
account's `max_unfinalized_credit`; architecture §17) and a passed restore drill
([RESTORE.md](RESTORE.md)); then [onboard](#operator-onboarding) Phala's own accounts with
`charges_enabled` (third-party merchants only after the legal review, design §17).

### Sealing the secrets

The CVM's encrypted env holds exactly the names of [staging.env.example](staging.env.example),
the same in both Environments: `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY` (the R2 token),
`SENTRY_DSN` (empty turns Sentry off), and `TOPUP_RPC_PROVIDER_A_KEY`, `TOPUP_RPC_PROVIDER_B_KEY`:
the API key topup puts in place of `{key}` in the provider's attested URL, empty for a keyless URL
(staging's). A key is at least 8 characters of `A-Z a-z 0-9 - . _ ~`; topup refuses a provider whose
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

A CVM sealed before the RPC keys existed allows only the first three names, and Deploy `upgrade`
keeps a CVM's allowed names, so its attestation check would refuse the upgrade. Once, before that
upgrade, run only the `envs update` above with `.env.ENV` holding all five names (the keys empty for
keyless URLs): the running compose ignores the two new names, and preflight against it would refuse
them.

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
| `AWS_ENDPOINT`, `WALG_S3_PREFIX`, `TOPUP_ADMIN_PUBLIC_KEY`, `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | the Environment variables of the same name |
| `AWS_REGION`, `AWS_S3_FORCE_PATH_STYLE`, `TOPUP_ADMIN_KID`, `SENTRY_ENVIRONMENT` | derived ([One-time setup](#one-time-setup-human-only-repository-owner), step 4) |
| `TOPUP_DOMAIN`, `TOPUP_GATEWAY_DOMAIN` | the Environment variable, and the CVM node's gateway: `dstack-ingress`'s `DOMAIN` and `GATEWAY_DOMAIN`; topup's `TOPUP_PUBLIC_ORIGIN` is `https://$TOPUP_DOMAIN` |
| `TOPUP_IMAGE`, `POSTGRES_WALG_IMAGE` | the release's digests; the image digest is also the Sentry release |
| `TOPUP_RESTORE_FROM_BACKUP`, `TOPUP_SERVICE_ENABLED` | the variant: service `off`, `on`; `--restore-check`: `on`, `read-only` |
| ingress | the variant: the service runs `dstack-ingress` on 443 and publishes no topup port; `--restore-check` runs no ingress and publishes topup on 8081 (blocks after `# only-in: VARIANT` in the compose) |
| `dstack-ingress` image | pinned in [docker-compose.yml](docker-compose.yml) by digest ([Custom domain](#custom-domain)) |
| route files (inline configs) | committed in [docker-compose.yml](docker-compose.yml), checked against `config/routes/` by [validate-compose.sh](validate-compose.sh) |

Every service also carries the label `phala-pay.rendered-sha256`, so any rendered change
recreates it. To change a setting, change the variable (or the route, by PR) and run Deploy
`upgrade`. [product/render-compose.sh](product/render-compose.sh) renders the product the same way.

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

**HUMAN-ONLY, owner of the domain's Cloudflare zone**, once per CVM instance. Every topup Deploy
run lists the records, in the tls-alpn-01 format of the pinned README:

| Type | Name | Content |
|---|---|---|
| CNAME | `$TOPUP_DOMAIN` | `$TOPUP_GATEWAY_DOMAIN` |
| TXT | `_dstack-app-address.$TOPUP_DOMAIN` | `<instance_id>:443` |
| CAA (optional) | `$TOPUP_DOMAIN` | `0 issue "letsencrypt.org;validationmethods=tls-alpn-01;accounturi=<ACME account>"` |

- DNS only (grey cloud): a proxied name resolves to Cloudflare, so neither the CA nor a client
  reaches the gateway.
- The TXT names the instance, not the app: the CA's validation must reach the one instance that
  holds the ACME order. An upgrade keeps the instance id; a new instance ([Resume](RESTORE.md#resume))
  serves the domain only after the TXT carries its id.
- Until both records resolve, dstack-ingress serves a self-signed placeholder and requests no
  certificate, so Deploy's `/healthz` wait on the domain fails.
- CAA is optional. An existing CAA record on the domain or `phala.com` must permit `tls-alpn-01`
  (`phala.com` has none as of 2026-09-26). Pinning `accounturi` to the account that
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
deployment and the route daily. Mainnet repeats this after the security review
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
   (umask 077 && admin POST /v1/admin/accounts "$(jq -cn '{name: "Phala Cloud",
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

These are the merchant's steps, done with its own secret key; for Phala's own accounts (the staging
reference product, Phala Cloud) Phala's staff do them as the merchant, never with the admin key.
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

## Staging reset (HUMAN-ONLY)

The multi-tenant schema (design §14) replaced the migration history and migrates no data
(`crates/topup/migrations/README.md`), and staging's route now uses the new factory, so the staging
service is replaced, not upgraded: a new CVM on an empty backup prefix, with every account created
again. Nothing on staging is live, so no funds or merchants are affected. Every step below is
HUMAN-ONLY except the workflow runs, which the staging owner dispatches; agents and CI run none of
them. In order:

1. **Merge and build.** This change is on `main`; run Release images and note its run id.
2. **Deploy the factory on Sepolia** ([Contracts](#contracts)) and confirm `verify-deployment.sh`
   passed and Verify contracts is green.
3. **Treasury Safe.** Its owners set the staging finance Safe's (`0x936c…4504`) fallback handler to
   the `CompatibilityFallbackHandler` (a Safe transaction `setFallbackHandler(0xfd0732Dc9E303f09fCEf3a7388Ad10A83459Ec99)`),
   without which it cannot prove itself as a treasury; then a PR updates `fallback_handler` in
   [contracts/safe-expectations.json](contracts/safe-expectations.json) and
   `deploy/contracts/verify-safe.sh` passes on both providers.
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
   that `GET /v1/config` with any test key lists the route at version 3 with `confirmations` 2.
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

## Staging reference product

Staging's reference product is a merchant like any other, with its own account, and a second CVM
running [product/reference_product](product/reference_product): `serve` mode is the webhook
receiver that applies every `deposit.*` snapshot by the balance rule (a deposit nets to
`amount − amount_refunded − amount_reversed` while `credited` or `reversed`; its tests are in
`product/tests`), an account API, and the public demo, with a SQLite ledger; `deposit` mode, run
from an operator's machine, plays a customer and signs the product's account API with a separate
driver key (`driver/v1`, the product's own authentication, not Phala Pay's).

- **Its key.** The sealed env holds only `PRODUCT_API_KEY`, the account's **restricted** test key
  (`ppay_rk_test_…`) with exactly the permissions it uses, listed in
  [product/staging.env.example](product/staging.env.example): `account.read`, `quotes.write`,
  `deposit_addresses.write`, `deposits.read`, `refunds.write`, `sweeps.read`, `forwarders.read`.
  Its preflight ([product/preflight.sh](product/preflight.sh)) accepts only a test key, restricted
  or secret. The account's secret key stays with the staging owner, offline.
- **Its pins.** The attested product config names its `account` (`acct_…`), the forwarder factory
  and implementation, and its Sepolia treasury, from which the SDK recomputes every quote and
  deposit address before the product shows it; the placeholder `acct_000…` fails the online
  preflight until a PR sets the real id. It pins its account's test-mode webhook keys from the
  authenticated attestation at `TOPUP_ORIGIN`, fetched with `PRODUCT_API_KEY`, at startup or on the
  first webhook when the key is sealed later (until then it answers `503`, and topup retries).
- **Attested settings.** `TOPUP_ORIGIN` (`https://$TOPUP_DOMAIN`), `PRODUCT_PUBLIC_URL` (its own
  gateway URL), `PRODUCT_RPC_URL` (a keyless Sepolia RPC: it is published and the product seals no
  RPC key), and `PRODUCT_DRIVER_PUBLIC_KEY`.

The product also serves the public **Phala Pay demo** at `PRODUCT_PUBLIC_URL/demo/`
([product/web](product/web), built into the image; served by
[reference_product/demo.py](product/reference_product/demo.py)): a cloud console's billing page
with both ways to collect a payment: a **quote** (a locked price and an exact amount, paid through
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
browser gets a random demo account in an `HttpOnly` cookie; quote creation is rate-limited per
account (3 a minute, 20 a day) and overall (30 a minute), and the page carries a strict CSP. Test
PHA is minted by the visitor's own wallet (`mint` is public on the staging token), with Sepolia ETH
from a public faucet for gas. `cd product/web && pnpm run build && pnpm run e2e` runs the whole
flow on Anvil, with the real factory at its deterministic address, against a stand-in service
([product/web/e2e/fake_service.py](product/web/e2e/fake_service.py)).

Setup, in order, after the [staging reset](#staging-reset-human-only)'s steps 1–8 (each step
**HUMAN-ONLY** unless it is a workflow run):

1. On the owner's machine (mode-0600 files, never committed), create the driver key and set the
   `staging` variable `PRODUCT_DRIVER_PUBLIC_KEY` (the driver's printed `public_key`);
   `PRODUCT_RPC_URL` is `TOPUP_RPC_PROVIDER_B_URL` unless the variable of that name is set:

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

3. **Treasury Safe owners**: set the account's Sepolia treasury to the finance Safe
   ([Treasury setup](#treasury-setup), Safe message; in test mode it applies at once). Open a PR
   setting the product config's `account` in [product/docker-compose.yml](product/docker-compose.yml)
   to the new `acct_…` id, and merge it.
4. Deploy (`staging`, target `product`, `provision`), set `STAGING_PRODUCT_CVM_ID`, and seal
   `.env.product` holding `PRODUCT_API_KEY=<ppay_rk_test_…>` with the two commands the summary
   prints. Then, with the secret key, register the product's endpoint:
   `POST /v1/webhook_endpoints {"url": "<PRODUCT_PUBLIC_URL>/webhooks", "enabled_events": ["*"]}`
   and `POST /v1/webhook_endpoints/{id}/test`.
5. Run a deposit. The payer is a Foundry keystore with a throwaway key and some Sepolia ETH; the
   test PHA token is a `MockERC20` with a public `mint`, so the driver mints the quoted amount and
   pays it. `driver.json` holds the `ProductConfig` fields: `service_url` (topup's origin),
   `account`, `route`, `chain_id`, `rpc_url`, `factory`, `implementation`, `treasury` (the
   account's Sepolia treasury), `token`, `token_symbol`, and `public_url` (the product URL), with
   the route's values.

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

### Abnormal paths

The driver also plays the sandbox scenarios' abnormal payments against staging, each in a fresh
workspace, and checks the deposit state, the verified webhooks, and the product ledger
(architecture §7, §9, §15):

| Path | Options | Expected |
|---|---|---|
| underpayment | `--pay-bps 9700` | `credited` at spot for what arrived, then `swept`; the lock later expires |
| after the quote window | `--pay-after-expiry` | `quote.expired`, then `credited` at spot and `swept` |
| unsupported token | `--token T --until rejected` | once confirmed `rejected(unsupported_asset)`; the tokens stay in the forwarder; `TopupUnsupportedInflows` |
| refund | `--pay-bps N --until refunded --refund-to A` | a payment of N/10000 of the quote above `max_deposit_atomic` (200000 test PHA): `rejected(out_of_bounds)`, swept; the driver requests a refund and waits while the treasury Safe's owners, as the merchant, pay it from the treasury of the deposit's address and attaches the transaction with `POST /v1/refunds/{id}/mark_paid`, until `succeeded` and one `deposit.refunded` |

Each row adds its options to the step-5 driver command: `T` is the Sepolia unsupported test token
`0x287E3577c66866a3F5Cb7a8Dac6761EB43608392`, `A` an address the staging owner controls, and the refund
row needs `--timeout 43200`. A mismatch between the expected and the observed outcome exits
non-zero with the reason.

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
