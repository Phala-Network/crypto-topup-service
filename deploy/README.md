# dstack staging deployment

This directory implements work packages D1 and D2 from `docs/plan.md`. It prepares images and
deployment artifacts; it does not deploy a CVM. D3 encrypted backups, restore, and restore drills
are documented in [RESTORE.md](RESTORE.md).

Every command that changes a registry, Phala Cloud, a CVM, a Safe, an on-chain contract, or secret
state is marked **HUMAN-ONLY**. The commands were checked on 2026-09-22 against dstack commit
`721df1b93fd93884224f2261c37dd86ca250432f` and Phala Cloud CLI `phala` 1.1.22; the SDK and the
simulator now pin the `v0.6.0-rc5` release commit `ad92cfeb4ab6960275498c31b66004b9bb1df068`, whose
SDK and linked documents are identical to that commit.

## First staging deploy checklist

Human inputs, all required before step 1 below:

| Input | Used for |
|---|---|
| Phala Cloud workspace to deploy into, with the CLI 1.1.22 logged in to it | `phala deploy`, `preflight.sh --workspace` |
| Registry push credentials for `ghcr.io/phala-network` | publishing `crypto-topup` and `postgres-walg` |
| Sepolia deployer key with test ETH, imported as a Foundry keystore account | canonical proxy (if absent), forwarder factory, test PHA token, sanctions oracle |
| Finance/test Safe on Sepolia (admin and treasury), with its owners, threshold, and Safe version for `deploy/contracts/safe-expectations.json` | factory constructor, `verify-safe.sh`, `verify-deployment.sh` |
| Base provisioner key with ETH, a Base RPC URL, and the Finance/test Safe on Base | the `DstackApp` contract `phala deploy` creates, then its ownership |
| Two Sepolia RPC URLs from different providers | `TOPUP_RPC_PROVIDER_A_URL`, `TOPUP_RPC_PROVIDER_B_URL` |
| S3-compatible bucket, endpoint, region, and keys | `AWS_*`, `WALG_S3_PREFIX` |
| Coin Metrics API key, or empty for the community endpoint | `COINMETRICS_API_KEY` |
| Admin keypair (`topup-sdk keygen`); only the key id and public key go into the env | `TOPUP_ADMIN_KID`, `TOPUP_ADMIN_PUBLIC_KEY` |

Owner decisions, also before step 1: the staging `settlement_url` and `product_kid` in
`deploy/config/routes/phala-cloud-sepolia-pha.yaml` currently name the production Phala Cloud
endpoint; confirm or change them (new route file content, reviewed like any attested change).
The OS image must be a dstack 0.6.0 build (step 6).

Steps, in order. Nothing before step 8 touches Phala Cloud; `make cvm-rehearsal` has already run
steps 2-5 and the post-boot checks locally against Anvil, MinIO, and the dstack simulator (see
[Local verification](#local-verification)).

1. **HUMAN-ONLY, Sepolia deployer key and Finance Safe:** commit the approved Safe in
   `deploy/contracts/safe-expectations.json`, then deploy and verify the factory exactly as
   [CONTRACTS.md](CONTRACTS.md#sepolia) describes. Deploy the test PHA token and sanctions oracle
   (there is no PHA token or Chainalysis oracle on Sepolia) and keep the JSON manifest:

   ```sh
   cast wallet import staging-deployer --interactive
   # Foundry reads ETH_PASSWORD as the path of a password file, not the password itself.
   ETH_PASSWORD_FILE=$(mktemp) && chmod 600 "$ETH_PASSWORD_FILE"
   read -rsp "Keystore password: " pw && printf '%s' "$pw" > "$ETH_PASSWORD_FILE" && unset pw && printf '\n'
   ETH_PASSWORD="$ETH_PASSWORD_FILE" deploy/sandbox/deploy-test-contracts.sh \
     --rpc-url "$SEPOLIA_RPC_A" --account staging-deployer > sepolia-test-contracts.json
   rm -f "$ETH_PASSWORD_FILE"
   ```

2. Route PR: copy `factory` and `implementation` from `sepolia-contract-verification.json`, the
   treasury Safe, `test_token` as `asset.contract`, and `sanctions_oracle` into
   `deploy/config/routes/phala-cloud-sepolia-pha.yaml` and its inline copy in
   `deploy/docker-compose.yml` (the `topup_route_phala_cloud_sepolia_pha` config), then check and
   merge through review:

   ```sh
   deploy/validate-compose.sh
   docker run --rm -i crypto-topup-service:dev topup route validate /dev/stdin \
     < deploy/config/routes/phala-cloud-sepolia-pha.yaml
   ```

3. **HUMAN-ONLY, registry credentials:** from the merged commit, build, push, and read back both
   images as in [Build and publish images](#build-and-publish-images), and export `TOPUP_IMAGE`
   and `POSTGRES_WALG_IMAGE` as `repository@sha256:<platform manifest digest>`.
4. Fill `.env.staging` (ignored by Git) from the example. Every name stays; only
   `AWS_SESSION_TOKEN`, `AWS_ENDPOINT` (AWS S3), and `COINMETRICS_API_KEY` may be empty. The
   gateway URL is not known before provisioning, so start with the provisional origin (step 9):

   ```sh
   install -m 600 deploy/staging.env.example .env.staging
   openssl rand -hex 32   # POSTGRES_PASSWORD, and again for TOPUP_APP_PASSWORD
   # DATABASE_URL=postgres://topup_service:<TOPUP_APP_PASSWORD>@postgres:5432/topup
   # MIGRATE_DATABASE_URL=postgres://postgres:<POSTGRES_PASSWORD>@postgres:5432/topup
   # TOPUP_PUBLIC_ORIGIN=https://pending.invalid
   ```

5. Render the compose: `deploy/render-compose.sh > deploy/docker-compose.staging.yml`.
6. Choose the OS image. The service's pinned dstack SDK (0.6.0 at the `v0.6.0-rc5`
   release commit `ad92cfe`) calls the `dstack.guest.v1` API at `/v1`, which pre-0.6 guest agents
   do not serve, so the image must be dstack 0.6.0, preferably `0.6.0-rc5`. Pick a non-dev name that the KMS
   contract allows:

   ```sh
   npx --yes phala@1.1.22 os-images --prod --all --json | jq '.items[] | {name, slug, version}'
   export KMS_CONTRACT=0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C
   npx --yes phala@1.1.22 kms base --json | jq --arg kms "$KMS_CONTRACT" '.contracts[]
     | select((.contract_address | ascii_downcase) == ($kms | ascii_downcase))
     | .os_images[] | select(.on_chain_allowed)'
   export OS_IMAGE=<allowed dstack 0.6.0 name>
   ```

7. Run the preflight, which is read-only against remote systems (it builds the contracts and
   pulls the images locally); it must pass before `phala deploy`. It checks the env file (all
   names, no `replace-me`, consistent database URLs), the compose (identical to a fresh render of
   the checkout with the same images, digest images, variables equal to the env names, a route
   without placeholder addresses), both images in the registry,
   `topup route validate`, both RPC providers' chain id, `verify-deployment.sh` against those
   providers with the route's factory/implementation/treasury, token and oracle code, the
   logged-in workspace (by display name, best effort), and that the KMS contract allows a device
   and `$OS_IMAGE`, a non-dev dstack 0.6 image:

   ```sh
   deploy/preflight.sh --env .env.staging --compose deploy/docker-compose.staging.yml \
     --workspace "$PHALA_WORKSPACE" --os-image "$OS_IMAGE"
   ```

8. **HUMAN-ONLY:** provision with the command in
   [A. First-time provisioning](#a-first-time-provisioning).
9. **HUMAN-ONLY:** set the real public origin and read back the attestation, as described at the
   end of section A. Only then issue product credentials.

## Deployment artifacts

- `docker-compose.yml` is the measured workload. Render immutable image references into a separate
  staging file before giving it to the CLI.
- `staging.env.example` lists every encrypted environment variable. `MIGRATE_DATABASE_URL` is sent
  only to the one-shot `migrate` service; `topup` receives only the app-role `DATABASE_URL`.
- `app-compose.example.json` and `render-app-compose.sh` are review previews of the fields CLI
  1.1.22 constructs. They are not authoritative deployment manifests or authorization artifacts.
- `verify-attested-compose.sh` compares a deployed attestation manifest with the exact rendered
  compose and the compose hash reported for the CVM.
- `Dockerfile.postgres-walg` supplies PostgreSQL 18 plus WAL-G and the D3 wrappers for encrypted,
  key-versioned WAL archiving and restore; see [RESTORE.md](RESTORE.md) for the procedure and drills.
- `alerts/prometheus-rules.yml` and `dashboards/crypto-topup-service.json` are the Prometheus and
  Grafana artifacts for §16. `GET /metrics` is intentionally unauthenticated and is served on the
  separate `--metrics-bind` listener (default `127.0.0.1:9464`). The measured compose binds that
  listener to the container network on port 9464 with `expose`; it is not published through the
  port-8080 gateway. Only the monitoring collector may reach it. The local compose publishes it on
  loopback port 19464 for smoke testing. `make alerts-check` (`check-alerts.sh`) runs
  `promtool check rules` and the alert unit tests in `alerts/prometheus-rules.test.yml` with the
  pinned Prometheus image.

## Service startup checks

`topup run` refuses to start until every RPC provider of every route shows the route's factory,
`implementation()`, `treasury()`, `factory()`, `addressOf(sample salt)`, and the recorded
contract code (architecture §4, §14). The check runs before the service touches the database, so
an outage of any single configured provider blocks restarts by design; a running service is not
affected. Restore the provider or wait for it; do not remove it from the route to get past the
check, since the route is attested.

Two routes that name the same product must agree on `destination.settlement_url` and
`destination.product_kid`, or startup fails. The `restore-check` tools service pins one route
file (`phala-cloud-sepolia-pha.yaml`) in its entrypoint, so adding a second route or product also
requires adding that route to `restore-check` and to the `topup run` command in the compose.

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

The service image must be published by the same reproducible build path that is verified locally:

```sh
export SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
deploy/verify-image.sh
```

The script performs two clean BuildKit OCI exports for `linux/amd64`, with provenance and SBOM
attachments disabled and `rewrite-timestamp=true`, then compares their OCI manifest and config
digests. This proves repeatability on the current builder and platform, not cross-builder or
cross-architecture identity.

**HUMAN-ONLY, registry credentials required:** set a candidate tag and let the same script build,
push, read back, and compare the registry child manifest and config with both verified local builds:

```sh
export PUBLISH_IMAGE=ghcr.io/phala-network/crypto-topup:staging-candidate
deploy/verify-image.sh
docker buildx imagetools inspect "$PUBLISH_IMAGE"
```

Use the reported platform manifest digest as `TOPUP_IMAGE`; do not deploy the tag or assume a
registry index digest equals its platform manifest digest. Registry-added indexes or attestations
can legitimately change the outer index digest while the child manifest and config stay identical.

Build and publish PostgreSQL/WAL-G separately:

```sh
docker build -f deploy/Dockerfile.postgres-walg \
  -t ghcr.io/phala-network/postgres-walg:16-3.0.9 .
docker run --rm ghcr.io/phala-network/postgres-walg:16-3.0.9 wal-g --version
```

**HUMAN-ONLY, registry credentials required:** push that image, inspect its digest, and set
`POSTGRES_WALG_IMAGE` to `repository@sha256:...`.

Render literal, nonzero image digests into the compose. Secret values remain `${NAME:-}` references:

```sh
export TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:<64-hex-digest>
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:<64-hex-digest>
deploy/render-compose.sh > deploy/docker-compose.staging.yml
deploy/validate-compose.sh
docker compose -f deploy/docker-compose.staging.yml config >/dev/null
```

Keep `.env.staging` outside Git, based on `staging.env.example`. Both database URLs are encrypted,
but the compose enforces their separate consumers. Validate the route with real contract addresses
and without `--template` before deployment.

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

`compose-hash.sh APP_COMPOSE_JSON` canonicalizes a read-back manifest for independent comparison; it
must not be used to guess the CLI's pre-deployment manifest.

## A. First-time provisioning

This staging workload uses on-chain KMS on Base mainnet, chain ID 8453. The Sepolia chain in the
route is the asset chain and is independent of the KMS control plane. On 2026-09-22, CLI 1.1.22
reported the Base KMS contract as `0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C`; re-query it and
confirm the selected contract has allowed devices and an allowed production OS image:

**HUMAN-ONLY, Phala Cloud credentials required:** authenticate before querying the operator's
available KMS control plane:

```sh
npx --yes phala@1.1.22 login --no-open
npx --yes phala@1.1.22 kms base --json > base-kms.json
jq '{chain_id, contracts: [.contracts[] | {contract_address, devices, os_images}]}' base-kms.json
export KMS_CONTRACT=0x2f83172A49584C017F2B256F0FB2Dca14126Ba9C
```

**HUMAN-ONLY, Phala Cloud, Base RPC, registry, and provisioner-key credentials required:** authenticate
and load `PRIVATE_KEY` and `ETH_RPC_URL` from the operator's secret manager; the CLI reads both
from the environment, so neither appears on a command line. Do not add `--prepare-only`; it does
not halt the create path in this CLI version. Run [preflight.sh](preflight.sh) first (checklist
step 7) and pass the same `$OS_IMAGE`: without `--image` the platform picks the OS image, and a
pre-0.6 image cannot serve the service's dstack SDK.

Phala CLI 1.1.22 writes `Provisioning CVM ...` to stdout before writing the pretty-printed JSON
object, and its deploy command has no separate output-file option. Preserve the complete stdout for
the deployment record, then extract from the first JSON object line and validate it before reading
any fields. See the pinned
[create handler](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts#L750-L752),
[JSON response](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts#L869-L887),
and [deploy options](https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/command.ts#L373-L377).

```sh
npx --yes phala@1.1.22 deploy --json \
  --name crypto-topup-staging \
  --compose deploy/docker-compose.staging.yml \
  -e .env.staging \
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
deploy/verify-attested-compose.sh \
  attestation.json cvm.json deploy/docker-compose.staging.yml
```

If the attestation is unavailable or its `compose_file` cannot be read back, stop. Do not enable the
route or represent the locally previewed manifest as the deployed artifact.

### Public origin follow-up

The workload booted with the provisional `TOPUP_PUBLIC_ORIGIN=https://pending.invalid`, which
passes startup validation, but every request signed for the real URL is rejected until the
origin is updated; during this window only the admin key exists, and no product credentials may
be issued. Derive the gateway URL of port 8080 from the provisioned app, confirm it serves the
service, and only then write it into `.env.staging`. CLI 1.1.22 `cvms get --json` (API
2026-06-23) reports the gateway domain as `gateway.base_domain`; every extraction below stops on
a missing field instead of writing a URL containing `null`:

```sh
GATEWAY_DOMAIN=$(jq -er '.gateway.base_domain' cvm.json) || exit 1
TOPUP_PUBLIC_ORIGIN="https://${APP_ID#0x}-8080.$GATEWAY_DOMAIN"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/healthz" || exit 1
sed -i "s|^TOPUP_PUBLIC_ORIGIN=.*|TOPUP_PUBLIC_ORIGIN=$TOPUP_PUBLIC_ORIGIN|" .env.staging
export TOPUP_PUBLIC_ORIGIN
deploy/preflight.sh --env .env.staging --compose deploy/docker-compose.staging.yml --offline
```

Encrypted env values are not part of the app-compose, so changing a value with the same set of
names keeps the compose hash. In CLI 1.1.22 `envs update` sends the re-encrypted values with the
name list; the API answers `in_progress` unless the name list changed, and only then asks for an
on-chain compose-hash registration signed with `PRIVATE_KEY`. Unset the key so an accidental name
change fails instead of registering a new hash:

**HUMAN-ONLY, Phala Cloud credentials required:**

```sh
env -u PRIVATE_KEY npx --yes phala@1.1.22 envs update "$CVM_ID" -e .env.staging
npx --yes phala@1.1.22 ps "$CVM_ID"
```

When `topup` is running again, repeat the read-back above (the compose hash must be unchanged),
confirm that a correctly signed administrative request is accepted (see
[Attestation, ingress, and egress](#attestation-ingress-and-egress)), and request an attestation
through the public URL:

```sh
export NONCE="$(openssl rand -hex 32)"
curl -fsS "$TOPUP_PUBLIC_ORIGIN/v1/attestation?nonce=$NONCE" > public-attestation.json
jq -e --arg nonce "$NONCE" '.keyid == "settlement/v1" and (.quote | length > 0)' \
  public-attestation.json
```

Bind `report_data` to `sha256(nonce ‖ settlement_pubkey)` and verify the quote as described below
before any product pins the settlement key.

## B. Upgrade an existing CVM

Use the same literal compose and env file for prepare and commit. The commit token binds the update,
so any compose/env change requires a new prepare.

**HUMAN-ONLY, Phala Cloud and encrypted-env credentials required:** prepare against the existing CVM:

```sh
export CVM_ID=<existing-cvm-id>
npx --yes phala@1.1.22 deploy --json \
  --cvm-id "$CVM_ID" \
  --compose deploy/docker-compose.staging.yml \
  -e .env.staging \
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
deploy/verify-attested-compose.sh \
  attestation.json cvm.json deploy/docker-compose.staging.yml
```

After the observation window, **HUMAN-ONLY, Finance Safe required:** remove the old hash with
`removeComposeHash(bytes32)`. Rollback is another upgrade: prepare the retained old compose, approve
its returned hash if necessary, commit its token, and repeat attestation checks. Never roll a schema
back destructively; use a forward repair migration.

## Attestation, ingress, and egress

Request an application-bound quote with a fresh nonce:

```sh
export NONCE="$(openssl rand -hex 32)"
npx --yes phala@1.1.22 ssh "$CVM_ID" -- \
  sh -lc "docker exec \"\$(docker ps -q --filter label=com.docker.compose.service=topup)\" \
  topup attest --nonce '$NONCE'"
```

The output also reports `operator_keyid` and `operator_address` for `--operator-key-version`
(default 1). dstack derives keys from the application identity rather than the compose hash, so
the current deployment can report the next operator address before it is used. To rotate the
operator key:

1. Run the command above with `--operator-key-version <next>` to read the new operator address.
2. **HUMAN-ONLY, admin Safe required:** `grantRole(OPERATOR_ROLE, <new operator>)` on each
   chain's factory.
3. Fund the new operator address with native gas on each chain.
4. Bump `operator_key_version` in the attested chain and route files, then upgrade as above (new
   compose hash, allow-list, deploy). Every current route on one chain must share the version.
5. Confirm the `flusher operator holds OPERATOR_ROLE` log line with the new version; unsigned
   plans are re-bound to the new operator and in-flight flushes of the old one keep confirming.
6. **HUMAN-ONLY, admin Safe required:** once no flush of the old operator is in flight, revoke
   its `OPERATOR_ROLE`.

A flusher whose operator lacks the role plans and sends nothing, logs an error and raises an
`OperatorRoleMissing` alert at every maintenance interval, and resumes by itself once the role
is granted.

**HUMAN-ONLY, verifier approval required:** verify the platform certificate/quote and TCB in the
Phala Trust Center or official dstack verification flow, replay the RTMR event log, confirm the
attested compose hash, and bind the fresh nonce to the returned `settlement/v1` public key.

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
storage host, attested product settlement host, DNS, and required Phala/dstack platform endpoints.
Record resolved hostnames, ports, and enforcement rules. Do not treat `pre_launch_script` as the
firewall boundary because it runs after Docker startup.

Before enabling a route, also confirm real route addresses validate without template mode, both RPC
providers agree at `finalized`, Safe/factory/implementation/CREATE2 checks pass, migrations completed,
WAL archiving is current, the product pins the attested settlement key, pilot limits are approved,
and a restore drill per [RESTORE.md](RESTORE.md) has passed.

## Local verification

The local stack is the attested `deploy/docker-compose.yml` with the `deploy/local/docker-compose.yml`
overlay, which adds MinIO, the dstack simulator, and a mock product, builds the images from the
checkout, and replaces secrets, ports, and host paths with local values. It builds dstack's
simulator from the pinned source revision and shares its `/var/run/dstack.sock` with `topup`. Pass
both files to any manual command:

```sh
docker compose -f deploy/docker-compose.yml -f deploy/local/docker-compose.yml ps
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
docker compose -f deploy/docker-compose.yml -f deploy/local/docker-compose.yml down -v
```

`infra-smoke.sh` tests migrations, route-template validation, simulator attestation, the running
backup service, WAL-G dry-run commands, and PostgreSQL archive settings, then removes its containers
and volumes. `service-smoke.sh` is opt-in and requires `topup run --help` to expose the unified
`--bind` and `--route` options. It starts the service with the application database role, local
administrative verification key, mounted route, and scanner provider URL; then it checks the TCP
listener, `GET /healthz` for HTTP 200, `GET /openapi.json`, and the running backup service. The local
provider URL is deliberately unreachable, exercising scanner retry behavior without contacting a
real chain. A skipped service smoke is not a successful service check.

Backup encryption, MinIO object storage, point-in-time recovery, and the weekly destructive drill
are documented in [RESTORE.md](RESTORE.md). Run `make restore-drill`; it uses an isolated Compose
project and removes all drill containers and volumes on exit.

### CVM rehearsal

`make cvm-rehearsal` ([local/cvm-rehearsal.sh](local/cvm-rehearsal.sh)) runs the staging
artifact itself rather than the local overlay: it pushes both images to a throwaway loopback
registry, deploys the factory (mock Safe as admin and treasury), test token, and sanctions oracle
to an Anvil chain with Sepolia's chain id using the A2 and sandbox scripts, writes the staging
route with those addresses into a copy of the compose, renders it with `render-compose.sh`, and
starts it with a `.env` holding exactly the `staging.env.example` names.
[local/cvm-rehearsal.compose.yml](local/cvm-rehearsal.compose.yml) adds only the dstack simulator
(in place of the host socket), MinIO, and Anvil. The run asserts that `migrate` exits 0, `topup`
passes its startup contract check and serves `/healthz`, `/v1/attestation` binds a fresh nonce
through the simulator, the backup marker is fresh, and one quote-first deposit is credited end to
end against the reference product with a lock priced from the live HTTPS sources (so the image's
TLS verification with system roots works), then prints the workload's memory and checks that no
container, volume, network, or image of the run is left. It needs Foundry with `contracts/lib`, the
Docker host's loopback (for the registry and Anvil), and internet access for the live price
sources; it bind-mounts nothing.

## Pinned upstream references

- dstack boundaries and encrypted env:
  <https://github.com/Dstack-TEE/dstack/blob/ad92cfeb4ab6960275498c31b66004b9bb1df068/docs/security/cvm-boundaries.md>
- dstack socket, gateway, and simulator usage:
  <https://github.com/Dstack-TEE/dstack/blob/ad92cfeb4ab6960275498c31b66004b9bb1df068/docs/usage.md>
- dstack verification:
  <https://github.com/Dstack-TEE/dstack/blob/ad92cfeb4ab6960275498c31b66004b9bb1df068/docs/verification.md>
- Phala CLI 1.1.22 deploy implementation:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts>
- Phala CLI 1.1.22 flags:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/command.ts>
- dstack `DstackApp` authorization contract:
  <https://github.com/Dstack-TEE/dstack/blob/ad92cfeb4ab6960275498c31b66004b9bb1df068/dstack/kms/auth-eth/contracts/DstackApp.sol>
