# dstack staging deployment

This directory implements work packages D1 and D2 from `docs/plan.md`. It prepares images and
deployment artifacts; it does not deploy a CVM. D3 backup encryption, restore automation, and
restore drills remain separate work.

Every command that changes a registry, Phala Cloud, a CVM, a Safe, an on-chain contract, or secret
state is marked **HUMAN-ONLY**. The commands were checked on 2026-09-22 against dstack commit
`721df1b93fd93884224f2261c37dd86ca250432f` and Phala Cloud CLI `phala` 1.1.22.

## Deployment artifacts

- `docker-compose.yml` is the measured workload. Render immutable image references into a separate
  staging file before giving it to the CLI.
- `staging.env.example` lists every encrypted environment variable. `MIGRATE_DATABASE_URL` is sent
  only to the one-shot `migrate` service; `topup` receives only the app-role `DATABASE_URL`.
- `app-compose.example.json` and `render-app-compose.sh` are review previews of the fields CLI
  1.1.22 constructs. They are not authoritative deployment manifests or authorization artifacts.
- `verify-attested-compose.sh` compares a deployed attestation manifest with the exact rendered
  compose and the compose hash reported for the CVM.
- `Dockerfile.postgres-walg` supplies PostgreSQL 16 plus WAL-G. Its hooks prepare D3 but do not claim
  encrypted backups, a tested restore, or an achieved RPO/RTO.

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
and load `PRIVATE_KEY` and `ETH_RPC_URL` from the operator's secret manager. Do not add
`--prepare-only`; it does not halt the create path in this CLI version.

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
  --private-key "$PRIVATE_KEY" \
  --rpc-url "$ETH_RPC_URL" \
  --no-dev-os \
  --no-public-logs \
  --no-public-sysinfo \
  --public-tcbinfo \
  --secure-time \
  --wait > provision.raw
sed -n '/^{/,$p' provision.raw > provision.json
jq -e . provision.json >/dev/null
export CVM_ID="$(jq -er '.vm_uuid' provision.json)"
export APP_ID="$(jq -er '.app_id' provision.json)"
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
export COMPOSE_HASH="$(jq -er '.compose_hash' prepare.json)"
export COMMIT_TOKEN="$(jq -er '.commit_token' prepare.json)"
export APP_ID="$(jq -er '.app_id' prepare.json)"
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

dstack app-compose has no hostname egress allow-list. **HUMAN-ONLY, cloud network authority
required:** restrict outbound access to the two RPC hosts, configured price-source hosts, object
storage host, attested product settlement host, DNS, and required Phala/dstack platform endpoints.
Record resolved hostnames, ports, and enforcement rules. Do not treat `pre_launch_script` as the
firewall boundary because it runs after Docker startup.

Before enabling a route, also confirm real route addresses validate without template mode, both RPC
providers agree at `finalized`, Safe/factory/implementation/CREATE2 checks pass, migrations completed,
WAL archiving is current, the product pins the attested settlement key, pilot limits are approved,
and D3 limitations are accepted explicitly.

## Local verification

The local stack builds dstack's simulator from the pinned source revision and shares its
`/var/run/dstack.sock` with `topup`.

```sh
make up
make down
make infra-smoke
SERVICE_SMOKE=1 deploy/local/service-smoke.sh
```

`infra-smoke.sh` tests migrations, route-template validation, simulator attestation, the running
backup service, WAL-G dry-run commands, and PostgreSQL archive settings, then removes its containers
and volumes. `service-smoke.sh` is opt-in and requires `topup run --help` to expose the unified
`--bind` and `--route` options. It starts the service with the application database role, local
administrative verification key, mounted route, and scanner provider URL; then it checks the TCP
listener, `GET /healthz` for HTTP 200, `GET /openapi.json`, and the running backup service. The local
provider URL is deliberately unreachable, exercising scanner retry behavior without contacting a
real chain. A skipped service smoke is not a successful service check.

## Pinned upstream references

- dstack boundaries and encrypted env:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/security/cvm-boundaries.md>
- dstack socket, gateway, and simulator usage:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/usage.md>
- dstack verification:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/verification.md>
- Phala CLI 1.1.22 deploy implementation:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/handler.ts>
- Phala CLI 1.1.22 flags:
  <https://github.com/Phala-Network/phala-cloud/blob/c22252e4afb82051a8008aa41ac72fa0a731aa26/cli/src/commands/deploy/command.ts>
- dstack `DstackApp` authorization contract:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/dstack/kms/auth-eth/contracts/DstackApp.sol>
