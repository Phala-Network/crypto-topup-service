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

1. reads its balance of `token` (ETH when `token` is zero); a token's `balanceOf` is a
   `staticcall` with `BALANCE_OF_GAS` (30 000) gas, and one that reverts, runs out of gas, or
   returns fewer than 32 bytes emits `FlushFailed` and deploys nothing;
2. skips it if it holds nothing, deploying nothing;
3. deploys the clone if it has no code, emitting `ForwarderCreated(salt, forwarder, treasury)`;
4. calls the forwarder's `flush(token)` with `FLUSH_GAS` (200 000) gas through a low-level call
   that copies at most 256 bytes of revert data;
5. emits `Flushed(salt, forwarder, token, treasury, amount)` on success, or
   `FlushFailed(salt, forwarder, token, reason)` and continues with the next salt.

Every external call a target makes is bounded, so one failing target (a token blacklisting the
forwarder or the treasury, a treasury refusing ETH, a token or treasury hook that reverts or burns
its gas) never blocks the others, as with Multicall3's `allowFailure`, and costs the batch at most
its bounds. ETH is sent with at most `NATIVE_SEND_GAS` (50 000) gas and without copying return
data. The factory's `flush` is non-reentrant (`ReentrancyGuardTransient`).

The bounds are part of the contract, not a caller's choice:

- Before each bounded call the factory checks that the call receives its whole bound (the 63/64
  rule); otherwise `flush` reverts with `InsufficientGas`. A caller's gas limit therefore cannot
  make an honest target fail: a transaction short of gas reverts as a whole, and estimating gas
  with `eth_estimateGas` accounts for it.
- `FLUSH_GAS` limits what one forwarder's `flush` may use: the token's `balanceOf` and
  `transfer`, including any hook the token calls on the treasury. Measured with cold state
  (`forge test -vv --mt GasBound`), a standard OpenZeppelin ERC-20 (PHA-like) uses 58 981, a
  USDC-like and a USDT-like token behind an EIP-1967 proxy 70 521 and 72 693, and ETH to a new
  account or a Safe-like proxy 59 335 and 60 714. A token whose transfer needs more than
  200 000 gas (for example a heavy ERC-777 or ERC-1363 hook on the treasury) fails every target
  with `FlushFailed` and cannot be swept through this factory; such tokens must not be enabled
  in a route.
- `BALANCE_OF_GAS` limits the factory's `balanceOf` read; the same tokens use at most 7 297.

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
(reverting or gas-burning treasury, blacklisting token, revert-data bomb, a `balanceOf` that
reverts, returns short data, or burns gas, a `transfer` or treasury hook that burns gas) leaves
the others' transfers intact, a caller short of gas reverts instead of failing a target, standard
tokens fit the gas bounds, and reentrant tokens and treasuries cannot reenter the factory.

The first flush for a salt includes the clone deployment; later flushes reuse it. Use
`forge snapshot` after contract changes to update `.gas-snapshot`.

Generate the cross-language vectors (`test-vectors/create2.json`: raw salts, quote salts, and
deposit address salts, read by `crates/core` and the Python SDK) with:

```sh
forge script script/GenerateCreate2Vectors.s.sol:GenerateCreate2Vectors
```

### Safe v1.4.1 (test-only)

`lib/safe-smart-account` is `safe-global/safe-smart-account` at tag `v1.4.1` (commit `bf943f8`),
LGPL-3.0, used only by the service's integration tests (`crates/topup/tests/treasuries.rs`): none
of it is compiled into the service or into the contracts deployed here. The `safe` profile of
`foundry.toml` builds it as Safe released it (solc 0.7.6, optimizer off, per its CHANGELOG); the
tests deploy the singleton, `SafeProxyFactory`, `CompatibilityFallbackHandler`, and
`SignMessageLib` on Anvil, check that their code equals the canonical v1.4.1 deployments' (without
the Solidity metadata, which names source paths), and prove treasuries with Safe owners' EIP-712
`SafeMessage` signatures and `SignMessageLib` approvals, as the Safe{Core} SDK produces them.

```sh
FOUNDRY_PROFILE=safe forge build lib/safe-smart-account/contracts/Safe.sol
```
