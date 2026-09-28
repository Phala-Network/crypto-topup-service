# Deterministic contract deployment

This runbook implements work package A2. No signing key is stored in CI: every funding or
broadcast command below is **HUMAN-ONLY**, run by a deployer with their own key, and the
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
keccak256("phala-pay.ForwarderFactory.v2")
= 0x26f1d8427b0c2db52d02ee55402198e592a278fb8541ba4dfaefbd1ea7b09eee
```

`ForwarderFactory` has no constructor arguments, no roles, and no admin
([design D3](../docs/design/multi-tenant.md#d3-contracts)). Its init code is the build's creation
code, so the CREATE2 address depends only on the build and the salt: the factory is
`0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747` and its implementation (the factory's first CREATE)
`0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9` on every chain (`local-test-vectors.json` records
both for the committed build). Anyone can deploy it; a treasury is chosen per forwarder, not per
factory, so a treasury change never needs a new factory.

## Prerequisites

- Foundry `1.8.3`, `jq`, and the repository dependencies.
- Two RPC providers for each chain used by post-deployment verification.
- A funded deployment EOA. Keep `PRIVATE_KEY` only in the operator's environment or secret manager.

`deploy/contracts/networks.json` maps each target network name to its chain id; Sepolia and
mainnet are prefilled. Targets are written `NETWORK[/LABEL]=URL`; `NETWORK` selects the expected
chain id, which the RPC's `eth_chainId` must report, and the optional label distinguishes providers
in the report.

### Treasury Safe

Route files name no treasury: treasuries are the accounts' own, each proven per chain and mode
through the API by its merchant with an EIP-4361 (EOA) or EIP-1271 (Safe) signature, and every
forwarder commits to the treasury it pays
([design D10](../docs/design/multi-tenant.md#d10-treasury-proof-and-changes);
[Treasury change](runbooks/treasury-change.md)). The service never checks a Safe's configuration.
`verify-safe.sh` remains a check of one Safe: the `treasury` in
`deploy/contracts/safe-expectations.json`, Phala's finance Safe, which Phala's finance proves as the
treasury of Phala Cloud's account. Run it before that proof and whenever the Safe's owners change
(the Verify contracts workflow runs it daily on Sepolia). On every target it checks: the RPC's
`eth_chainId` equals the committed chain id for the target network, the address has code (an EOA is rejected), the proxy
runtime code hash is approved, storage slot 0 and `masterCopy()` both equal the approved singleton,
the singleton's runtime code hash matches, owners match as a set, the threshold matches exactly,
the enabled modules (`getModulesPaginated`) match as a set, and the guard and fallback handler
storage slots of Safe v1.4.1 hold exactly the approved addresses. The singleton check matters
because every Safe proxy has the same runtime code; only slot 0 decides which implementation
answers `getOwners()` and `getThreshold()` and executes transactions. Modules, the guard, and the
fallback handler matter because a module can execute from the Safe without the owners'
signatures, a guard can block Safe transactions, and the fallback handler answers calls the Safe
does not implement itself.

```sh
deploy/contracts/verify-safe.sh \
  --rpc sepolia/a="$SEPOLIA_RPC_A" \
  --rpc sepolia/b="$SEPOLIA_RPC_B"
```

If any Safe check fails, Phala's finance does not prove the Safe as a treasury (or moves the
treasury off it) until the Safe or the reviewed expectations are corrected.

## Reproducible build

`expected-codehashes.json` records the pinned compiler profile, canonical proxy hash, fixed salt,
and build artifact hashes. Runtime template hashes intentionally exclude immutable substitutions
(the factory's `implementation`, the implementation's `factory`); the deployment and verification
scripts create a temporary local reference deployment to derive exact factory and implementation
runtime hashes. The file
also records each immutable's 32-byte word offsets (`immutable_offsets`). At startup `topup run`
requires the route's addresses at exactly those offsets and, with them zeroed, the runtime template
hash; its compiled-in copies of these values are tested against this file. It also compares the
factory's `addressOf(treasury, sample salt)` with its own derivation.

```sh
export PATH="$HOME/.foundry/bin:$HOME/.cargo/bin:$PATH"
deploy/contracts/check-build.sh --check
make deploy-check
```

If an intentional contract or compiler-profile change is approved, regenerate and review the
fingerprints with `deploy/contracts/check-build.sh --write` and the local deployment vectors with
`deploy/contracts/test-determinism.sh --write`. Never regenerate them merely to make a failed
deployment check pass. A contract change needs the independent review before mainnet
([architecture §4](../docs/architecture.md#4-contracts)).

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

The staging route (`deploy/config/routes/phala-cloud-sepolia-pha.yaml`) expects the #202 build's
deterministic factory `0x45466D37587E6E46DC35eB96b74ba3D3b1E5b747` and implementation
`0x49F2F1F1a25269Ea0C6FF2AB1C7B09dCBE9c5bA9`. Until they are deployed on Sepolia, `topup run`
refuses to start with the route (its startup contract check reads the factory's
`implementation()`). Deploying them is a **HUMAN-ONLY** step of
the staging reset (`deploy/README.md`, "Staging reset"), before the Deploy `upgrade` that ships
the route.

**HUMAN-ONLY**, with the deployer key in the environment only (the forge script reads
`PRIVATE_KEY`; it never appears in argv):

```sh
read -rsp "Deployer private key: " PRIVATE_KEY && printf '\n' && export PRIVATE_KEY

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
implementation addresses before it sends. If the predicted factory already has code (anyone may
have deployed it), it accepts only the exact runtime hashes derived from the local build.

`verify-deployment.sh` checks, on every target, the chain id, the proxy, factory, and
implementation runtime code hashes, `implementation()`, the implementation's `factory()`, and
`addressOf(treasury, salt)` for every sample forwarder of `contracts/test-vectors/create2.json`.

## Mainnet

Repeat only after Sepolia verification and the human release approval. **HUMAN-ONLY:**

```sh
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
implementation, every sample forwarder, and runtime code hashes must be identical.

## Route and compose update

A route file carries only `chain.forwarder_factory`; the implementation is derived as the
factory's first CREATE (`chain.implementation` is optional), and at startup `topup run` requires
the factory's `implementation()` to be it and both runtime code hashes to match the build. Put the
verified factory into the route file under `deploy/config/routes/` and the identical inline copy in `deploy/docker-compose.yml` (its
`configs`); `deploy/validate-compose.sh` fails if the inline copy differs from the file. Create a
new route version; never mutate the contract tuple of an enabled version. Then run:

```sh
deploy/validate-compose.sh
# Export the attested settings first: the Environment's variables
# (deploy/README.md, "Attested settings").
deploy/render-compose.sh > deploy/docker-compose.staging.yml
docker compose -f deploy/docker-compose.staging.yml config >/dev/null
```

Deploy the merged route with the Deploy workflow in mode `upgrade` (`deploy/README.md`, "Deploy"),
which verifies the attested read-back. A locally
rendered compose is only a review aid. Keep both contract verification reports with the deployment
record.

## Rollback and treasury changes

There is no contract rollback, upgrade, or setter. A failed or superseded deployment remains on
chain; correct the code and deploy a new factory under a new salt and route version, leaving
historical versions available for existing deposits. A treasury change needs no new factory and
no route change: treasuries are the accounts', set through the API
([Treasury change](runbooks/treasury-change.md)); new forwarders are derived for the new treasury,
and existing forwarders keep paying theirs. Phala's finance Safe is Phala Cloud's account's
treasury, verified here and proven through the API like any merchant's.
