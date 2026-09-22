# dstack deployment

This directory implements work packages D1 and D2 from `docs/plan.md` and the deployment model in
`docs/architecture.md` sections 5 and 14. It does not deploy a CVM. Every step that changes cloud,
chain, Safe, registry, or secret state is marked **HUMAN-ONLY**.

The commands below were checked on 2026-09-22 against dstack commit
`721df1b93fd93884224f2261c37dd86ca250432f` and Phala Cloud CLI `phala` 1.1.22. The old
`Phala-Network/phala-cloud-cli` repository is archived; the maintained CLI is in
`Phala-Network/phala-cloud` and is published as the `phala` npm package.

## Files

- `docker-compose.yml`: measured staging workload. Replace both image placeholders with registry
  digests before computing the compose hash.
- `app-compose.example.json`: dstack manifest policy, including the exact encrypted environment
  variable allow-list and gateway port restriction.
- `config/`: authoring copies of the Sepolia chain and route templates. Production compose embeds
  these and the role initializer with `configs.content`, so their bytes are inside the measured
  `docker_compose_file`; `validate-compose.sh` rejects drift between the copies.
- `Dockerfile.postgres-walg`: PostgreSQL 16.15 plus WAL-G 3.0.9.
- `local/`: local Postgres and dstack guest-agent simulator stack.

## Build and verify images

Build the service from the source commit timestamp:

```sh
export SOURCE_DATE_EPOCH="$(git log -1 --pretty=%ct)"
docker build --build-arg SOURCE_DATE_EPOCH="$SOURCE_DATE_EPOCH" \
  -t ghcr.io/phala-network/crypto-topup:staging .
docker run --rm ghcr.io/phala-network/crypto-topup:staging topup --help
deploy/verify-image.sh
```

`verify-image.sh` performs two clean BuildKit OCI exports with provenance/SBOM disabled, rewrites
layer timestamps to `SOURCE_DATE_EPOCH`, and compares both the OCI manifest and image config
digests. This verifies the committed build path on the current builder and platform. It does not
prove cross-builder or cross-architecture identity, and registry-added attestations can give the
pushed multi-platform index a different digest.

Build the database image and check WAL-G:

```sh
docker build -f deploy/Dockerfile.postgres-walg \
  -t ghcr.io/phala-network/postgres-walg:16-3.0.9 .
docker run --rm ghcr.io/phala-network/postgres-walg:16-3.0.9 wal-g --version
```

**HUMAN-ONLY, registry credentials required:** push both images, then record immutable digests:

```sh
docker push ghcr.io/phala-network/crypto-topup:staging
docker push ghcr.io/phala-network/postgres-walg:16-3.0.9
docker buildx imagetools inspect ghcr.io/phala-network/crypto-topup:staging
docker buildx imagetools inspect ghcr.io/phala-network/postgres-walg:16-3.0.9
```

Set `TOPUP_IMAGE` and `POSTGRES_WALG_IMAGE` to `repository@sha256:...`; never deploy tags.

## Configure staging

1. Replace all address placeholders in `config/chains/ethereum-sepolia.yaml` and
   `config/routes/phala-cloud-sepolia-pha.yaml`. Keep their values aligned.
2. Validate the route template, then validate it again without template mode after real addresses
   are present:

   ```sh
   docker run --rm -v "$PWD/deploy/config:/etc/topup/config:ro" "$TOPUP_IMAGE" \
     topup route validate --template \
     /etc/topup/config/routes/phala-cloud-sepolia-pha.yaml
   docker run --rm -v "$PWD/deploy/config:/etc/topup/config:ro" "$TOPUP_IMAGE" \
     topup route validate /etc/topup/config/routes/phala-cloud-sepolia-pha.yaml
   ```

3. Create a non-committed env file containing every name in `allowed_envs`. Database URLs should
   use the owner for `MIGRATE_DATABASE_URL` and `topup_service` for `DATABASE_URL`. Object storage
   variables are used by PostgreSQL continuous archiving and the backup service.
4. **HUMAN-ONLY, provider/object-storage/product credentials required:** confirm the credentials
   are staging-scoped, have minimum permissions, and are not present in the compose or shell
   history.

WAL-G hooks are present, but D3 is not complete: the operator must not claim encrypted backups or
an RPO/RTO until `backup/v1` key wrapping, restore-check, and the weekly restore drill are
implemented and exercised.

## Egress and ingress

`app-compose.example.json` enables the dstack gateway and restricts gateway ingress to CVM port
8080. The expected TLS endpoint is the Phala/dstack gateway URL for port 8080; PostgreSQL has no
published port.

The dstack app-compose schema does not provide a hostname egress allow-list. **HUMAN-ONLY, cloud
network authority required:** before enabling a route, enforce outbound access at the CVM/network
layer to only:

- the two hosts extracted from `RPC_PROVIDER_A_URL` and `RPC_PROVIDER_B_URL`;
- Coin Metrics, Binance, and Kraken hosts selected by the adapters;
- the object-storage endpoint in `AWS_ENDPOINT`/`WALG_S3_PREFIX`;
- the product host from the attested route `destination.settlement_url`;
- DNS and platform endpoints required by the selected Phala Cloud/dstack environment.

Record the resolved hostnames, ports, and the enforcement mechanism in the deployment ticket.
Do not put a firewall security gate in `pre_launch_script`: dstack documents that it runs after
Docker and cannot reliably constrain restored containers before they start.

## Compose hash and allow-list

Render the example app-compose with the exact compose text and compute dstack's deterministic
SHA-256 (recursive key sorting, compact JSON):

```sh
deploy/render-app-compose.sh > deploy/app-compose.staging.json
COMPOSE_HASH="0x$(deploy/compose-hash.sh)"
printf '%s\n' "$COMPOSE_HASH"
```

The rendered file is public attestation input and contains no secrets. Run
`deploy/validate-compose.sh` before rendering, and recompute after any image, route, chain, policy,
port, or compose change.

**HUMAN-ONLY, finance Safe/on-chain credentials required:** submit the following calldata to the
app's dstack `DstackApp` contract and wait for finality:

```sh
export APP_AUTH_CONTRACT=0x...
cast calldata 'addComposeHash(bytes32)' "$COMPOSE_HASH"
cast call "$APP_AUTH_CONTRACT" 'allowedComposeHashes(bytes32)(bool)' "$COMPOSE_HASH" \
  --rpc-url "$SEPOLIA_RPC_URL"
```

The calldata must be executed by the contract owner, normally through the Safe. The final `cast
call` must return `true` before deployment.

## Deploy with Phala Cloud

Use the pinned CLI without installing it globally:

```sh
npx phala@1.1.22 --version
```

**HUMAN-ONLY, Phala Cloud credentials required:** authenticate and prepare the deployment. The
prepare step is the authoritative cloud-side check of the compose and registered hash:

```sh
npx phala@1.1.22 login --no-open
npx phala@1.1.22 deploy \
  --name crypto-topup-staging \
  --compose deploy/docker-compose.yml \
  --env .env.staging \
  --instance-type tdx.medium \
  --fs ext4 \
  --no-public-logs \
  --no-public-sysinfo \
  --public-tcbinfo \
  --secure-time \
  --prepare-only
```

Compare the prepared compose hash with `$COMPOSE_HASH`. After the Safe transaction is final,
commit using the token and transaction hash printed by the prepare command:

```sh
npx phala@1.1.22 deploy \
  --commit \
  --token "$PHALA_COMMIT_TOKEN" \
  --compose-hash "$COMPOSE_HASH" \
  --transaction-hash "$ALLOWLIST_TRANSACTION_HASH" \
  --wait
npx phala@1.1.22 link crypto-topup-staging
npx phala@1.1.22 ps
npx phala@1.1.22 logs topup --tail 100
```

Passing `--env` encrypts the allowed variables client-side before upload. For later secret-only
rotation, use:

```sh
npx phala@1.1.22 envs update crypto-topup-staging --env .env.staging
```

## Verify attestation

Generate an application-bound quote through the mounted dstack v1 socket:

```sh
NONCE="$(openssl rand -hex 32)"
npx phala@1.1.22 ssh crypto-topup-staging -- \
  sh -lc "docker exec \"\$(docker ps -q --filter label=com.docker.compose.service=topup)\" \
  topup attest --nonce '$NONCE'"
```

Also retrieve the platform evidence and measured runtime configuration:

```sh
npx phala@1.1.22 cvms attestation crypto-topup-staging --json > attestation.json
npx phala@1.1.22 runtime-config crypto-topup-staging --json > runtime-config.json
```

**HUMAN-ONLY, verifier approval required:** use the Phala Trust Center or the official
`dstack-verifier`/DCAP verification flow to verify platform signatures and TCB, replay the runtime
event log, and confirm its compose hash equals `$COMPOSE_HASH`. Confirm the `topup attest` report
data binds the fresh nonce and returned `settlement/v1` public key, then pin `(keyid, public key)`
in the product verifier.

Before enabling the Sepolia route, also check: both RPC providers agree at `finalized`; factory,
implementation, treasury Safe, and sample CREATE2 address checks pass; the product accepts the
pinned settlement key; WAL archiving is current; egress enforcement is active; policy numbers and
pilot caps have human sign-off.

## Upgrade and rollback

Upgrade order:

1. Build and verify the new image, then pin its registry digest.
2. Update compose/config, render app-compose, and compute the new hash.
3. **HUMAN-ONLY:** add the new hash through the Safe and wait for finality.
4. **HUMAN-ONLY:** run `phala deploy --prepare-only`, verify the hash, then `--commit --wait`.
5. Verify migrations, health, route validation, attestation, reconciliation, and backup age.
6. **HUMAN-ONLY:** remove the old hash only after the observation window:

   ```sh
   cast calldata 'removeComposeHash(bytes32)' "$OLD_COMPOSE_HASH"
   ```

Rollback uses the same process: keep the previous digest and compose artifact, re-add its hash if
necessary, prepare/commit that compose, verify attestation, and only then remove the failed hash.
Never roll database schema backward destructively; use a forward repair migration.

## Local development

The local simulator image is built from the pinned dstack commit because that commit publishes no
simulator container image. It exposes the same `/var/run/dstack.sock` v1 surface through a shared
volume.

```sh
make up
make down
make smoke
```

`make smoke` builds the local images, starts Postgres and the simulator, applies migrations,
validates the route template, requests `topup attest --nonce deadbeef`, conditionally checks
`/healthz` when C9 is present, and always removes its containers and volumes.

## Verified upstream facts

- dstack app-compose fields, `allowed_envs`, encrypted env handling, and compose measurement:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/security/cvm-boundaries.md>
- dstack socket mount, gateway URL convention, and encrypted env usage:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/usage.md>
- official simulator source and fixtures:
  <https://github.com/Dstack-TEE/dstack/tree/721df1b93fd93884224f2261c37dd86ca250432f/sdk/simulator>
- dstack verification flow:
  <https://github.com/Dstack-TEE/dstack/blob/721df1b93fd93884224f2261c37dd86ca250432f/docs/verification.md>
- current Phala Cloud CLI deployment commands:
  <https://github.com/Phala-Network/phala-cloud/blob/main/cli/docs/deploy.md>
