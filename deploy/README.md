# dstack deployment

Every CVM is deployed by GitHub Actions from `main`; nothing is deployed from a laptop. Steps
outside the workflows that change a registry, Phala Cloud, a CVM, a Safe, a contract, or a secret
are marked **HUMAN-ONLY**. Backup and restore: [RESTORE.md](RESTORE.md). Incident response:
[runbooks](runbooks/README.md). Contracts: [CONTRACTS.md](CONTRACTS.md).

## What runs where

| Where | What | Deployed by |
|---|---|---|
| topup CVM, one per Environment (`staging`, `production`) | [docker-compose.yml](docker-compose.yml): `keys` (derives the database passwords and the backup key), `postgres` (PostgreSQL 18 + WAL-G), `migrate`, `topup` (the service, or read-only and published on 8081 in the [restore-check variant](RESTORE.md#the-restore-check-variant)), `dstack-ingress` (the only public port, 443: TLS for the [custom domain](#custom-domain), service variant only), `heartbeat`, `backup`, `restore-check` (acts only in that variant) | Deploy, target `topup` |
| Staging reference-product CVM | [product/docker-compose.yml](product/docker-compose.yml), port 8089 | Deploy, target `product` |
| Object storage (Cloudflare R2) | encrypted WAL-G base backups and WAL under `WALG_S3_PREFIX` | owner |
| Sentry project `phala-network/crypto-topup-service` | errors, alerts, Crons and Uptime monitors | the service itself |
| Ethereum (Sepolia for staging) | factory, forwarders, treasury Safe | Safe owner ([CONTRACTS.md](CONTRACTS.md)) |
| GitHub Actions | [Release images](../.github/workflows/release-images.yml), [Deploy](../.github/workflows/deploy.yml), [Verify contracts](../.github/workflows/verify-contracts.yml) (daily, read-only), [Restore drill](../.github/workflows/restore-drill.yml) (weekly, local stack) | — |

Production CVMs have no SSH, no logs, and no database access. Everything an operator sees comes
from the public endpoints, the admin API, Sentry, and the chain.

### KMS

Every CVM uses Phala Cloud's KMS (`--kms phala`, owner decision): no `DstackApp` contract and no
on-chain compose-hash approval. Fund safety does not depend on upgrade governance (forwarders pay
only the immutable treasury). Credits are what the attested service signs, and products pin its
key from attestation and may cap or verify credits on their own node. A malicious upgrade could
cause downtime, read service data, or sign false credits up to the product's caps; the compose
hash in the attestation, verified after every deploy, makes it detectable.

### OS image

The approved OS image is `dstack-0.5.9`, non-dev: the latest dstack release a Phala Cloud node
offers. [preflight.sh](preflight.sh) accepts only that name and, online, requires a node of the
workspace to offer it. The service speaks the dstack 0.5 guest API (`dstack-sdk = "=0.1.3"`); the
local simulator is built from the same release. dstack 0.6 derives different keys for the same
domain, so moving to it changes the operator address, settlement key, backup key, and database
passwords: that is a key migration, not an image bump.

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
   | `TOPUP_DOMAIN` | the [custom domain](#custom-domain): `crypto-topup-api-staging.phala.com` (`staging`), `crypto-topup-api.phala.com` (`production`) |
   | `AWS_ENDPOINT` | `https://<account>.r2.cloudflarestorage.com` |
   | `WALG_S3_PREFIX` | `s3://BUCKET/PATH`; a new app needs a prefix of its own ([RESTORE.md](RESTORE.md#bootstrap-from-backup)) |
   | `TOPUP_ADMIN_PUBLIC_KEY` | from `topup-sdk keygen --keyid admin/<Environment>-v1`, a separate key per Environment; the seed stays with the admin |
   | `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` | HTTPS RPC URLs of the route's chain from two different providers; they are published in the compose, so a provider that puts its API key in the URL is set with `{key}` in the key's place (`https://eth-mainnet.g.alchemy.com/v2/{key}`, `https://mainnet.infura.io/v3/{key}`, `https://NAME.quiknode.pro/{key}/`) and the key is sealed as `TOPUP_RPC_PROVIDER_A_KEY`/`_B_KEY` ([Sealing the secrets](#sealing-the-secrets)); preflight refuses a URL that embeds a key. The chain must carry the canonical Multicall3 ([contracts/multicall3.json](contracts/multicall3.json)) |
   | `STAGING_PRODUCT_CVM_ID`, `PRODUCT_DRIVER_PUBLIC_KEY` | `staging` only: [Staging reference product](#staging-reference-product) |

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
6. **Packages.** After the first Release images run, make `crypto-topup`, `postgres-walg`, and
   `crypto-topup-reference-product` public (organization Packages > package > Package settings >
   Change visibility; the organization must allow public container packages). This is
   irreversible. CVMs pull without credentials, and preflight fails on a private image.

## Release and deploy

### Build and publish images

Run **Release images** on `main` (Actions tab, or `gh workflow run release-images.yml --ref
main`). It builds `crypto-topup`, `postgres-walg`, and `crypto-topup-reference-product`, checks
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

**Production** additionally needs the mainnet contracts deployed by the Safe owner
([CONTRACTS.md](CONTRACTS.md#mainnet)) and a reviewed route PR putting the mainnet route into the
compose (Deploy refuses a `production` compose with any route off chain 1). After the first
deploy, in order: seal the secrets; [verify the attestation](#attestation-ingress-and-egress);
grant and fund the [flusher operator](#flusher-operator); [register the
product](#product-credentials); and have Finance, Risk, and Operations approve the pilot limits
(route bounds, lock-exposure caps, product-side caps; architecture §17) and a passed restore drill
([RESTORE.md](RESTORE.md)) before the product enables deposits.

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

Every service also carries the label `crypto-topup.rendered-sha256`, so any rendered change
recreates it. To change a setting, change the variable (or the route, by PR) and run Deploy
`upgrade`. [product/render-compose.sh](product/render-compose.sh) renders the product the same way.

### Custom domain

Products pin topup's origin in every signed `@target-uri`, so it is a stable name the owner
controls, `https://$TOPUP_DOMAIN`, not the CVM's gateway URL, which changes with the node and the
app id. The official [dstack-ingress](https://github.com/Dstack-TEE/dstack-examples/tree/dstack-ingress-v2.6/custom-domain/dstack-ingress)
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
  | `topup-scanner-<chain_id>` | after each finalized scan, every minute | 5 min |
  | `topup-pump-<n>`, `topup-outbox-<n>` | each iteration or poll, every minute | 5 min |
  | `topup-lock-expiry` | after each successful expiry scan, every minute | 5 min |
  | `topup-reconciler` | `ok` after a complete round, `error` after failed checks, every 10 min | 10 min |
  | `topup-backup` | `ok` while the WAL-G success marker is at most 120 s old, else `error`; 3 errors open an issue | 2 min |
  | `topup-flush-<route>` | on the route's `flush.schedule` (UTC): `ok` after planning, `error` when planning failed or the operator lacks `OPERATOR_ROLE` | 15 min |

- **Uptime**: `/healthz` of each Environment ([One-time setup](#one-time-setup-human-only-repository-owner)).
- **Egress**: `topup` sends HTTPS to the DSN's ingest host.

A restore-check instance runs no loop and reports as `<environment>-restore`, so it never checks
in or raises an alert of the live environment.

## Attestation, ingress, and egress

**HUMAN-ONLY, verifier**, before issuing product credentials. Deploy already verifies the attested
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

The settlement key and flusher operators come only from the public, nonce-bound attestation:

```sh
export NONCE="$(openssl rand -hex 32)"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/attestation?nonce=$NONCE" > public-attestation.json
jq '{quote: null, attestation: .quote}' public-attestation.json |
  deploy/dstack-verifier.sh > public-verification.json
jq -e --arg app "$(jq -r '.app_id | ltrimstr("0x") | ascii_downcase' cvm.json)" \
  --arg compose "$(jq -j '.compose_file' attestation.json | sha256sum | cut -d' ' -f1)" \
  --arg report_data "$(jq -r '.report_data' public-attestation.json)" '
  .details.tcb_status == "UpToDate" and .details.app_info.app_id == $app
  and .details.app_info.compose_hash == $compose
  and .details.report_data == $report_data + ("0" * 64)' public-verification.json
```

Then check that `report_data` binds the nonce, the `settlement/v1` key, and every operator with
the Python SDK's `topup_sdk.verify_attestation_binding(response, nonce)` (architecture §14
defines the construction; `TopupClient.attestation` runs it on every fetch).

**Ingress**: the attested compose must publish only `dstack-ingress` on 443; confirm `/openapi.json`
at `TOPUP_PUBLIC_ORIGIN` with a valid certificate, its [certificate
evidence](#custom-domain), and that PostgreSQL and topup's port 8080 are unreachable. The service
verifies every signed `@target-uri` against `TOPUP_PUBLIC_ORIGIN`, so a correctly signed request
answered `401` usually means the URL differs from it. **Egress** (HUMAN-ONLY, cloud network
authority; dstack has no hostname allow-list): restrict outbound traffic to the two RPC hosts,
the price sources, the object storage host, the product's webhook host, the Sentry ingest
host, DNS, and the Phala/dstack platform endpoints, and record the rules.

### Flusher operator

`operators` in the attestation lists, per chain, the key the flusher signs `flush` with
(`chain_id`, `operator_key_version`, `keyid`, `address`). It needs `OPERATOR_ROLE` on the chain's
factory and native gas; without the role the flusher sends nothing, raises `OperatorRoleMissing`,
and resumes by itself once granted. With an address from a verified response:

```sh
export OPERATOR_ADDRESS="$(jq -er --argjson chain "$CHAIN_ID" \
  '.operators[] | select(.chain_id == $chain) | .address' public-attestation.json)"
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
```

**HUMAN-ONLY, admin Safe:** execute the calldata on the factory and confirm
`cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"` is
`true`; fund the address ([gas refill](runbooks/gas-refill.md)).

Rotating the operator: bump `operator_key_version` in a new version of every current route on the
chain and Deploy `upgrade`; read the new address from a fresh verified attestation, grant and fund
it (new flushes wait for the grant; in-flight flushes of the old operator keep confirming), then
revoke the old role once none of its flushes is in flight. Emergency revocation:
[operator key compromise](runbooks/operator-key-compromise.md).

## Product credentials

`POST /v1/admin/products {"slug", "public_key", "webhook_url"}` is the only way to issue a
product. The product's key id is `{slug}/v1`, and the slug must be named by a loaded route (the
route's `product`), so the route change comes first. `public_key` is the base64 key the integrator printed with
`topup-sdk keygen`; `webhook_url` is an absolute `https` URL. The answer is `200` (also for a repeat
with the same values), `409` for the same slug with a different key or URL, or `400`.

`PUT /v1/admin/products/{slug} {"public_key", "webhook_url", "reason"}` replaces an issued
product's key and webhook URL: a hard cut, since requests are verified against the one stored key
under the key id `{slug}/v1`, which stays the same (architecture §15, Rotation). The answer is `200`
with the stored values (a repeat changes nothing), `404` for a slug never issued, or `400`; the
`audit` row `product.update` records the reason and the replaced values. A compromised key:
[product key compromise](runbooks/product-key-compromise.md).

**HUMAN-ONLY, admin key holder**, after [attestation](#attestation-ingress-and-egress): convert the
admin seed to PEM once, then sign and send the exact body:

```sh
(umask 077 && { printf '302e020100300506032b657004220420'; tr -d '\n' < admin.seed; } |
  xxd -r -p | openssl pkey -inform DER -out admin.pem)
export ADMIN_KEY_FILE=admin.pem ADMIN_KEY_ID=admin/staging-v1   # the CVM's TOPUP_ADMIN_KID
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

## Staging reference product

Staging's `phala-cloud` product is a second CVM running
[product/reference_product](product/reference_product): `serve` mode is the webhook receiver that
fulfills each `deposit.credited` once (its tests are in `product/tests`) and an account API, with
a SQLite ledger; `deposit` mode, run from an operator's machine, plays a Phala Cloud user and signs
with a separate driver key (`driver/v1`). Its sealed env holds only `PRODUCT_SEED`
([product/staging.env.example](product/staging.env.example)); `TOPUP_ORIGIN` (`https://$TOPUP_DOMAIN`,
for its API calls), `PRODUCT_PUBLIC_URL` (its own gateway URL), `PRODUCT_RPC_URL`, and
`PRODUCT_DRIVER_PUBLIC_KEY` are attested. At startup it pins topup's `settlement/v1` key, which
verifies the webhooks, from a verified attestation at `TOPUP_ORIGIN`. Its preflight
([product/preflight.sh](product/preflight.sh)) requires `PRODUCT_RPC_URL` to be a keyless Sepolia
RPC (it is published and the product seals no RPC key); the deposit driver pays through it. Switching staging to Phala Cloud's backend is a
`PUT /v1/admin/products/phala-cloud` with their key and webhook URL
([Product credentials](#product-credentials)); the route stays as it is. A product
CVM provisioned before its settings were attested still allows all five names: seal `.env.product`
with only `PRODUCT_SEED`, then Deploy `upgrade`, once.

The product also serves the public **Phala Pay demo** at `PRODUCT_PUBLIC_URL/demo/`
([product/web](product/web), built into the image; served by
[reference_product/demo.py](product/reference_product/demo.py)): a cloud console's billing page
paid with `@phala/pay`'s `<Checkout>`, with a live timeline of the payment built only from real
data (the service's quote and deposit read with the product key, this product's verified webhook
events and ledger rows, and the sweep transfer on chain), the product's signed API requests with
signatures shortened, and the service's attestation. Each browser gets a random demo account in an
`HttpOnly` cookie; quote creation is rate-limited per account (3 a minute, 20 a day) and overall
(30 a minute), and the page carries a strict CSP. It holds no faucet key: test PHA is minted by the
visitor's own wallet (`mint` is public on the staging token), with Sepolia ETH from a public faucet
for gas. `cd product/web && pnpm run build && pnpm run e2e` runs the whole flow on Anvil against a
stand-in service ([product/web/e2e/fake_service.py](product/web/e2e/fake_service.py)).

Setup, in order (each step **HUMAN-ONLY** unless it is a workflow run):

1. On the owner's machine (mode-0600 files, never committed), create the keys and set the
   `staging` variable `PRODUCT_DRIVER_PUBLIC_KEY` (the driver's printed `public_key`);
   `PRODUCT_RPC_URL` is `TOPUP_RPC_PROVIDER_B_URL` unless the variable of that name is set:

   ```sh
   cd sdk/python
   uv run --locked topup-sdk keygen --keyid phala-cloud/v1 --seed-out ~/staging/product.seed
   uv run --locked topup-sdk keygen --keyid driver/v1 --seed-out ~/staging/driver.seed
   ```

2. Deploy (`staging`, target `product`, `provision`), set `STAGING_PRODUCT_CVM_ID`, and seal
   `.env.product` holding `PRODUCT_SEED=<hex seed>` with the two commands the summary prints.
   Until then the account API answers 503.
3. Register `phala-cloud` in topup ([Product credentials](#product-credentials)) with the
   `phala-cloud/v1` public key and `<product URL>/webhooks`.
4. Run a deposit. The payer is a Foundry keystore with a throwaway key and some Sepolia ETH; the
   test PHA token is a `MockERC20` with a public `mint`, so the driver mints the locked amount and
   pays it. `driver.json` holds the `ProductConfig` fields: `service_url` (topup's origin),
   `product_slug`, `product_keyid`, `route`, `chain_id`, `rpc_url`, `factory`, `implementation`,
   `token`, `token_symbol`, and `public_url` (the product URL), with the route's values.

   ```sh
   export ETH_KEYSTORE=~/.foundry/keystores/staging-payer ETH_PASSWORD=~/staging/payer.password
   PYTHONPATH=deploy/product uv run --locked --project sdk/python python -m reference_product deposit \
     --config driver.json --driver-seed-file ~/staging/driver.seed \
     --amount-minor <cents> --min-atomic 20000000000000000000000
   ```

   The driver recomputes the lock address before paying and exits 0 once the product has
   recorded exactly one credit and the verified `deposit.credited` webhook (Sepolia finality takes
   about 15 minutes). The flusher sweeps only forwarders holding at least `min_flush_atomic`
   (20000 test PHA), so `--min-atomic` refuses a smaller quote and prints the `--amount-minor`
   needed; the quote must also fit the 500000-cent per-deposit and per-account caps (PHA below
   about $0.24). `--until swept --timeout 25200` also waits for the next sweep (schedule
   `0 */6 * * *` UTC).

`make cvm-rehearsal` runs this product CVM locally, with one deposit.

### Abnormal paths

The driver also plays the sandbox scenarios' abnormal payments against staging, each in a fresh
workspace, and checks the deposit state, the verified webhooks, and the product ledger
(architecture §7, §9, §15):

| Path | Options | Expected |
|---|---|---|
| underpayment | `--pay-bps 9700` | `credited` at spot for what arrived, then `swept`; the lock later expires |
| after the quote window | `--pay-after-expiry` | `quote.expired`, then `credited` at spot and `swept` |
| unsupported token | `--token T --until rejected` | after finality `rejected(unsupported_asset)`; the tokens stay in the forwarder; `TopupUnsupportedInflows` |
| refund | `--pay-bps N --until refunded --refund-to A` | a payment of N/10000 of the quote above `max_deposit_atomic` (200000 test PHA): `rejected(out_of_bounds)`, swept; the driver requests a refund and waits while it is executed as in [refund execution](runbooks/refund-execution.md), until `confirmed` and one `deposit.refunded` |

Each row adds its options to the step-5 driver command: `T` is the Sepolia unsupported test token
`0x287E3577c66866a3F5Cb7a8Dac6761EB43608392`, `A` an address the operator controls, and the refund
row needs `--timeout 43200`. A mismatch between the expected and the observed outcome exits
non-zero with the reason.

## Local verification

- `make up` / `make down`: the attested compose rendered with local settings plus the
  [local overlay](local/docker-compose.yml) (Garage S3, the dstack simulator, a mock product);
  run manual commands through `deploy/local/compose.sh`.
- `make cvm-rehearsal`: the staging artifact itself against Anvil, Garage, and the simulator,
  from the unsealed boot through sealing, operator grant, product registration, and one credited
  deposit.
- `make restore-drill`: [RESTORE.md](RESTORE.md#local-and-ci-drills).
- `make sandbox-local`: the integrator sandbox ([sandbox/README.md](sandbox/README.md)).
- `deploy/validate-compose.sh`: the compose policy CI enforces.
