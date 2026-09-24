# Deterministic contract deployment

This runbook implements work package A2. No signing key is stored in CI: every funding or
broadcast command below is **HUMAN-ONLY**, run by the Safe owner with their own key, and the
Verify contracts workflow re-checks the deployment daily. Mainnet is never deployed from
automation.

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
- Finance approval of the Safe version: its proxy runtime code hash, singleton address, and
  singleton runtime code hash; and of the Safe's enabled modules, guard, and fallback handler.

Before deployment, replace the intentionally unconfigured values in
`deploy/contracts/safe-expectations.json` and set `configured` to `true`. This file contains no
secrets and is the single source of truth for the factory constructor inputs:

- `networks`: every target network name and its chain id. Sepolia and mainnet are prefilled.
- `admin` and `treasury`: the factory `DEFAULT_ADMIN_ROLE` holder and the forwarder treasury.
  Each must match exactly one entry in `safes`; they may be the same Safe.
- `safes[]`: for each approved Safe, its `address`, `owners`, `threshold`, allowed
  `proxy_code_hashes`, `singleton`, `singleton_code_hash`, enabled `modules` (usually `[]`),
  `guard`, and `fallback_handler`. Use the zero address for "no guard" or "no fallback handler";
  a Safe created through the Safe UI normally has the `CompatibilityFallbackHandler` set.

`verify-safe.sh` checks each Safe on every target: the RPC's `eth_chainId` equals the committed
chain id for the target network, the address has code (an EOA is rejected), the proxy runtime code
hash is approved, storage slot 0 and `masterCopy()` both equal the approved singleton, the
singleton's runtime code hash matches, owners match as a set, the threshold matches exactly, the
enabled modules (`getModulesPaginated`) match as a set, and the guard and fallback handler storage
slots of Safe v1.4.1 hold exactly the approved addresses.
The singleton check matters because every Safe proxy has the same runtime code; only slot 0
decides which implementation answers `getOwners()` and `getThreshold()` and executes transactions.
Modules, the guard, and the fallback handler matter because a module can execute from the Safe
without the owners' signatures, a guard can block Safe transactions, and the fallback handler
answers calls the Safe does not implement itself.

Targets are written `NETWORK[/LABEL]=URL`; `NETWORK` selects the expected chain id and the optional
label distinguishes providers in the report. Verify the Safes independently on every chain:

```sh
deploy/contracts/verify-safe.sh \
  --rpc sepolia/a="$SEPOLIA_RPC_A" \
  --rpc sepolia/b="$SEPOLIA_RPC_B" \
  --rpc mainnet/a="$MAINNET_RPC_A" \
  --rpc mainnet/b="$MAINNET_RPC_B"
```

Do not deploy if any Safe check is false.

`deploy-factory.sh` and `verify-deployment.sh` share one parameter validation: `ADMIN` and
`TREASURY` from the environment must equal `admin` and `treasury` in the expectations file, and
both Safes must pass every check above on each target. Any inconsistency stops the script before
it simulates, broadcasts, or derives the reference deployment.

## Reproducible build

`expected-codehashes.json` records the pinned compiler profile, canonical proxy hash, fixed salt,
and build artifact hashes. Runtime template hashes intentionally exclude immutable substitutions;
the deployment and verification scripts create a temporary local reference deployment with the
actual constructor arguments to derive exact factory and implementation runtime hashes. The file
also records each immutable's 32-byte word offsets (`immutable_offsets`). At startup `topup run`
requires the route's addresses at exactly those offsets and, with them zeroed, the runtime template
hash; its compiled-in copies of these values are tested against this file.

```sh
export PATH="$HOME/.foundry/bin:$HOME/.cargo/bin:$PATH"
deploy/contracts/check-build.sh --check
make deploy-check
```

If an intentional contract or compiler-profile change is approved, regenerate and review the
fingerprints with `deploy/contracts/check-build.sh --write` and the local deployment vectors with
`deploy/contracts/test-determinism.sh --write`. Never regenerate them merely to make a failed
deployment check pass. A contract change reopens the no-external-audit decision in
[architecture §4](../docs/architecture.md#4-contracts).

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

Use the Finance Safe as both factory admin and treasury unless the committed expectations name
different approved Safes. `ADMIN` and `TREASURY` must match those entries. **HUMAN-ONLY**, with
the deployer key in the environment only (the forge script reads `PRIVATE_KEY`; it never appears
in argv):

```sh
read -rsp "Deployer private key: " PRIVATE_KEY && printf '\n' && export PRIVATE_KEY
export ADMIN="$(jq -er .admin deploy/contracts/safe-expectations.json)"
export TREASURY="$(jq -er .treasury deploy/contracts/safe-expectations.json)"

deploy/contracts/deploy-proxy.sh --rpc-url "$SEPOLIA_RPC_A"
deploy/contracts/deploy-factory.sh --rpc sepolia/a="$SEPOLIA_RPC_A" --dry-run
deploy/contracts/deploy-factory.sh --rpc sepolia/a="$SEPOLIA_RPC_A" --broadcast

deploy/contracts/verify-deployment.sh \
  --rpc sepolia/a="$SEPOLIA_RPC_A" \
  --rpc sepolia/b="$SEPOLIA_RPC_B" \
  > sepolia-contract-verification.json
jq -e '.passed == true' sepolia-contract-verification.json
```

The dry run and broadcast both hand `PRIVATE_KEY` to Foundry through the environment only:
`DeployFactory.s.sol` reads it with `vm.envUint("PRIVATE_KEY")` and passes it to
`vm.startBroadcast`, so the key is never a command-line argument (visible in the process list) and
never written to a repository file. The Foundry script prints the predicted factory and
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
deploy/contracts/deploy-factory.sh --rpc mainnet/a="$MAINNET_RPC_A" --dry-run
deploy/contracts/deploy-factory.sh --rpc mainnet/a="$MAINNET_RPC_A" --broadcast

deploy/contracts/verify-deployment.sh \
  --rpc mainnet/a="$MAINNET_RPC_A" \
  --rpc mainnet/b="$MAINNET_RPC_B" \
  > mainnet-contract-verification.json
jq -e '.passed == true' mainnet-contract-verification.json
```

Each report entry records the target, network, expected chain id, and the chain id the RPC
returned; a mismatch fails verification. Compare the Sepolia and mainnet JSON reports. Factory,
implementation, every sample forwarder, constructor inputs, and runtime code hashes must be
identical.

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
