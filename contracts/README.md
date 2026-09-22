# Forwarder contracts

This Foundry project implements the deterministic EIP-1167 deposit forwarders described in
`docs/architecture.md` sections 0, 2, 3, 4, 10, 13, and 16.

## Roles

- `DEFAULT_ADMIN_ROLE` is assigned only to the finance Safe supplied to the factory
  constructor. It grants and revokes operators.
- `OPERATOR_ROLE` is assigned to the service key. Operators may call the batch `flush`
  function but cannot change the treasury or implementation.
- A forwarder accepts ETH from any sender, but only its factory can flush ETH or ERC-20s.

## Deployment

1. Deploy and verify the treasury Safe at the intended address.
2. Deploy `ForwarderFactory(admin, treasury)`. Its constructor deploys the shared `Forwarder`
   implementation, binding both immutable addresses.
3. Grant `OPERATOR_ROLE` to the service key and verify `implementation()`, the implementation's
   `treasury()` and `factory()`, and `addressOf()` against the route configuration.

Deployment determinism depends on the compiler settings as well as constructor inputs. This
project pins Solidity `0.8.24`, EVM target `cancun`, optimizer settings, and metadata settings in
`foundry.toml`; deployments must use those checked-in settings so factory init code is identical.

Changing the treasury requires a new factory and a new route version. There is no setter,
owner, proxy upgrade, or per-clone mutable state.

## Flush semantics

Each salt is deployed on demand and flushed in order. A failed forwarder reverts the entire
batch with `ForwarderFlushFailed(salt, forwarder, reason)`. Atomic rollback avoids a partially
successful batch with incomplete receipt processing; the operator can retry the batch with a
fresh transaction as specified by the flusher recovery flow. If an address fails persistently,
the C7 flusher bisects the batch to isolate that address and alerts operators without weakening
the contract's atomic behavior.

`Flushed.amount` is the ETH sent or the increase in the treasury's ERC-20 balance. Plain,
verified ERC-20s are supported. Fee-on-transfer and rebasing tokens are unsupported; the test
suite documents the fee-on-transfer behavior, but such assets must not be enabled in a route.

## Gas notes

The first flush for a salt includes deterministic clone deployment. Later flushes reuse the
clone and are cheaper. Batching amortizes the factory call overhead, while each salt still pays
for balance checks, transfer execution, and one `Flushed` event. Use `forge snapshot` after
contract changes to update `.gas-snapshot`.

Generate Rust interoperability vectors with:

```sh
forge script script/GenerateCreate2Vectors.s.sol:GenerateCreate2Vectors
```
