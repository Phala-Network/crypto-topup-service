# Forwarder contracts

This Foundry project implements the deterministic deposit forwarders of
[design D3](../docs/design/multi-tenant.md#d3-contracts) and `docs/architecture.md` §4.

## Model

- `ForwarderFactory` has no roles, no admin, and no constructor arguments. Its constructor
  deploys the shared `Forwarder` implementation.
- Every deposit address is an EIP-1167 clone of the implementation created with OpenZeppelin
  `Clones.cloneDeterministicWithImmutableArgs`. The clone's only immutable argument is
  `abi.encodePacked(treasury)`, so its CREATE2 address commits to the factory, the
  implementation, the treasury, and the salt. `addressOf(treasury, salt)` predicts it with
  `Clones.predictDeterministicAddressWithImmutableArgs`.
- A forwarder accepts ETH and tokens from anyone. Only its factory can make it pay, and it can
  pay only its treasury (`Forwarder.treasury()` reads the clone argument).
- `flush(treasury, salts, token)` is public: the destination is fixed by the address, so the only
  effect of any call is moving funds to their owner.

## Flush semantics

For each salt, the factory derives the forwarder of `treasury` and:

1. skips it if it holds nothing of `token` (ETH when `token` is zero), deploying nothing;
2. deploys the clone if it has no code, emitting `ForwarderCreated(salt, forwarder, treasury)`;
3. calls the forwarder's `flush(token)` through a low-level call that copies at most 256 bytes of
   revert data;
4. emits `Flushed(salt, forwarder, token, treasury, amount)` on success, or
   `FlushFailed(salt, forwarder, token, reason)` and continues with the next salt.

One failing target (a token blacklisting the forwarder or the treasury, a treasury refusing ETH)
therefore never blocks the others, as with Multicall3's `allowFailure`. ETH is sent with at most
`NATIVE_SEND_GAS` (50 000) gas and without copying return data, so a treasury cannot consume the
batch's gas. The factory's `flush` is non-reentrant (`ReentrancyGuardTransient`).

`Flushed.amount` is the amount that left the forwarder: its whole balance. Plain ERC-20s are
supported, including tokens whose `transfer` returns nothing (USDT style); a `transfer` returning
`false` fails the target. Fee-on-transfer and rebasing tokens are unsupported and must not be
enabled in a route: the tests document that the treasury then receives less than `amount`.

## Deployment

`deploy/CONTRACTS.md` deploys the factory through the Arachnid deterministic deployment proxy with
the fixed salt `keccak256("phala-pay.ForwarderFactory.v2")`. Without constructor arguments the
factory and implementation have the same addresses on every chain, and anyone can deploy them.

Deployment determinism depends on the compiler settings. This project pins Solidity `0.8.37`, EVM
target `cancun`, optimizer settings, and metadata settings in `foundry.toml`; deployments must use
those checked-in settings so the init code is identical. `deploy/contracts/check-build.sh --check`
enforces those settings and the committed artifact fingerprints. `make deploy-check` also proves
the proxy, factory, implementation, and a forwarder resolve to identical addresses on two local
Anvil chains with different chain IDs.

There is no setter, owner, proxy upgrade, or per-clone mutable state.

## Tests

`forge test` runs unit, fuzz, and invariant tests: funds reach only the forwarder's own treasury,
`addressOf` equals the deployed clone, a zero-balance target deploys nothing, one failing target
(reverting or gas-burning treasury, blacklisting token, revert-data bomb) leaves the others'
transfers intact, and reentrant tokens and treasuries cannot reenter the factory.

The first flush for a salt includes the clone deployment; later flushes reuse it. Use
`forge snapshot` after contract changes to update `.gas-snapshot`.

Generate the cross-language vectors (`test-vectors/create2.json`, read by `crates/core` and the
Python SDK) with:

```sh
forge script script/GenerateCreate2Vectors.s.sol:GenerateCreate2Vectors
```
