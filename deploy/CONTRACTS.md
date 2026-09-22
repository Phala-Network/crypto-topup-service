# Deterministic contract deployment

This runbook implements work package A2. It prepares and verifies deployments; it does not perform
the Sepolia or mainnet deployment from automation. Every funding or broadcast command below is
**HUMAN-ONLY**.

The Arachnid deterministic deployment proxy is fixed at
`0x4e59b44847b379578588920cA78FbF26c0B4956C`. Its calldata is the plain 32-byte salt followed by
the complete init code. The upstream proxy itself is deployed by funding the one-time signer
`0x3fab184622dc19b6109349b94811493bf2a45362` and publishing the upstream signed legacy
transaction. These facts and the embedded transaction were checked on 2026-09-22 against upstream
commit `be3c5974db5028d502537209329ff2e730ed336c`.

The factory salt is fixed and unmodified:

```text
keccak256("crypto-topup-service.ForwarderFactory.v1")
= 0x33f357abc669d0dae6ca878fa2e4435dd82ff4a983efd8a8dd4f2efa9437a426
```

The CREATE2 address also commits to the full factory init code, including `admin` and `treasury`.
Those constructor arguments must be byte-for-byte identical on every chain. Changing the treasury
therefore produces a new factory address and requires a new route version.

## Prerequisites

- Foundry `1.8.3`, `jq`, and the repository dependencies.
- Two RPC providers for each chain used by post-deployment verification.
- A funded deployment EOA. Keep `PRIVATE_KEY` only in the operator's environment or secret manager.
- The Finance Safe deployed at the same address on every target chain with the same owners and
  threshold.
- Finance approval of the Safe implementation/version and its proxy runtime code hash.

Before deployment, replace the intentionally unconfigured values in
`deploy/contracts/safe-expectations.json` with the approved Safe address, owners, threshold, and
allowed Safe proxy runtime code hash. Set `configured` to `true`. Owners are compared as a set;
threshold and bytecode hash must match exactly. This file contains no secrets.

Verify the Safe independently on every chain:

```sh
deploy/contracts/verify-safe.sh \
  --rpc sepolia-a="$SEPOLIA_RPC_A" \
  --rpc sepolia-b="$SEPOLIA_RPC_B" \
  --rpc mainnet-a="$MAINNET_RPC_A" \
  --rpc mainnet-b="$MAINNET_RPC_B"
```

Do not deploy if any Safe check is false.

## Reproducible build

`expected-codehashes.json` records the pinned compiler profile, canonical proxy hash, fixed salt,
and build artifact hashes. Runtime template hashes intentionally exclude immutable substitutions;
the deployment and verification scripts create a temporary local reference deployment with the
actual constructor arguments to derive exact factory and implementation runtime hashes.

```sh
export PATH="$HOME/.foundry/bin:$HOME/.cargo/bin:$PATH"
deploy/contracts/check-build.sh --check
make deploy-check
```

If an intentional contract or compiler-profile change is approved, regenerate and review the
fingerprints with `deploy/contracts/check-build.sh --write`. Never regenerate them merely to make a
failed deployment check pass.

## Canonical proxy

Check a target chain first:

```sh
deploy/contracts/deploy-proxy.sh --rpc-url "$RPC_URL"
```

If the proxy is absent, the script prints the required funding command. **HUMAN-ONLY:** fund the
one-time signer with exactly the upstream transaction gas budget, then publish the fixed signed
transaction:

```sh
cast send 0x3fab184622dc19b6109349b94811493bf2a45362 \
  --value 0.01ether \
  --rpc-url "$RPC_URL" \
  --private-key "$PRIVATE_KEY"
deploy/contracts/deploy-proxy.sh --rpc-url "$RPC_URL" --broadcast
```

The script refuses an existing proxy whose runtime code hash is not the canonical hash. Some EVM
chains reject the unprotected legacy transaction; such a chain is unsupported until the
architecture explicitly selects another deterministic deployer.

## Sepolia

Use the Finance Safe as both factory admin and treasury unless an approved deployment record names
different Safe addresses. **HUMAN-ONLY:** after the Safe checks pass:

```sh
export ADMIN="$FINANCE_SAFE"
export TREASURY="$FINANCE_SAFE"
read -rsp "Deployment private key: " PRIVATE_KEY && printf '\n'
export PRIVATE_KEY

deploy/contracts/deploy-proxy.sh --rpc-url "$SEPOLIA_RPC_A"
deploy/contracts/deploy-factory.sh --rpc-url "$SEPOLIA_RPC_A" --dry-run
deploy/contracts/deploy-factory.sh --rpc-url "$SEPOLIA_RPC_A" --broadcast

deploy/contracts/verify-deployment.sh \
  --rpc sepolia-a="$SEPOLIA_RPC_A" \
  --rpc sepolia-b="$SEPOLIA_RPC_B" \
  > sepolia-contract-verification.json
jq -e '.passed == true' sepolia-contract-verification.json
```

The dry run and broadcast both pass `PRIVATE_KEY` to Foundry with the CLI `--private-key` option;
the key is never written to a repository file. The Foundry script prints the predicted factory and
implementation addresses before it sends. If the predicted factory already has code, it accepts
only the exact runtime hashes derived from the local build and constructor arguments.

## Mainnet

Repeat only after Sepolia verification and the human release approval. **HUMAN-ONLY:**

```sh
export ADMIN="$FINANCE_SAFE"
export TREASURY="$FINANCE_SAFE"
read -rsp "Deployment private key: " PRIVATE_KEY && printf '\n'
export PRIVATE_KEY

deploy/contracts/deploy-proxy.sh --rpc-url "$MAINNET_RPC_A"
deploy/contracts/deploy-factory.sh --rpc-url "$MAINNET_RPC_A" --dry-run
deploy/contracts/deploy-factory.sh --rpc-url "$MAINNET_RPC_A" --broadcast

deploy/contracts/verify-deployment.sh \
  --rpc mainnet-a="$MAINNET_RPC_A" \
  --rpc mainnet-b="$MAINNET_RPC_B" \
  > mainnet-contract-verification.json
jq -e '.passed == true' mainnet-contract-verification.json
```

Compare the Sepolia and mainnet JSON reports. Factory, implementation, every sample forwarder,
constructor inputs, and runtime code hashes must be identical.

## Route and compose update

Copy the verified `factory`, `implementation`, and `treasury` values into the chain configuration,
the route configuration, and the matching configuration embedded in `deploy/docker-compose.yml`.
Create a new route version; never mutate the contract tuple of an enabled version. Then run:

```sh
deploy/validate-compose.sh
deploy/render-compose.sh > deploy/docker-compose.staging.yml
docker compose -f deploy/docker-compose.staging.yml config >/dev/null
```

Follow `deploy/README.md` to obtain the authoritative prepared compose hash, approve that exact hash
with the Finance Safe, commit the update, and verify the attested read-back. A locally guessed hash
is not an authorization artifact. Keep both contract verification reports with the deployment
record.

## Rollback and treasury changes

There is no contract rollback, upgrade, or setter. A failed or superseded deployment remains on
chain. Correct the input or code, deploy a new factory, create a new route version, update the
attested compose, and leave historical versions available for existing deposits. A treasury change
always follows this new-factory/new-route process.
