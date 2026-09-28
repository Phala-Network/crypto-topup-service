# Wrong-network deposit

**Trigger:** a user reports sending tokens to their deposit address on another EVM chain (for example BNB
Chain, Arbitrum, Base, or Polygon) instead of Ethereum mainnet, usually because an exchange
withdrawal defaulted to a cheaper network. The service watches only configured chains, so no
deposit row, event, or credit exists for the payment.

**Impact:** one user's funds sit at the same address on the other chain. Deposit addresses are CREATE2
forwarder clones of the factory, and the factory is deployed through the canonical deterministic
deployment proxy with identical init code on every chain (see `deploy/CONTRACTS.md`), so the same
factory, implementation, and forwarder addresses can be reproduced on the other chain. Nobody,
including the service, can move the funds until Finance deploys the factory there; recovery is
manual, costs gas on that chain, and is never automatic or guaranteed. Other users and chains are
unaffected. The architecture's customer copy (§12) warns "Ethereum mainnet only" on every address.

## First steps

Support collects, through the agreed channel: the chain name and id, the transaction hash, the
token contract on that chain, the amount, and the workspace. The product confirms that the address
belongs to the workspace and supplies its `salt` (address responses carry it with its inputs);
check that the configured factory derives the address from it, then read the other chain:

```sh
export WRONG_CHAIN_RPC_URL=https://rpc.example-other-chain
export DEPOSIT_ADDRESS=0x... SALT=0x... WRONG_CHAIN_TOKEN=0x... TX_HASH=0x...
cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
cast chain-id --rpc-url "$WRONG_CHAIN_RPC_URL"
cast receipt "$TX_HASH" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast code "$FACTORY" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

Record the chain id, the balance still at the address, and whether the factory already has code
there. Tell the user the payment was not credited, that recovery is a manual Finance decision, and
that there is no automatic refund; never promise an outcome or a date.

## Decide

- Address not found, or it belongs to another workspace: stop; the funds are not at one of our
  addresses. Tell the user.
- Balance is zero: the funds already moved or never arrived there; ask for the transaction on an
  explorer and stop.
- The chain does not use standard CREATE2 addresses (for example zkSync Era) or rejects the
  deterministic deployment proxy's pre-EIP-155 transaction (`deploy/CONTRACTS.md`, Canonical
  proxy): the address cannot be reproduced with this factory; escalate to Engineering and Finance.
  Recovery may be impossible.
- The Finance Safe (admin and treasury) cannot be created at the same address on that chain: the
  forwarder could still be deployed, but it would flush to a treasury nobody controls there. Do not
  flush; escalate to Finance.
- Recoverable, and Finance decides the amount justifies the gas: continue with Remediation. Below
  that threshold, record the decision in the case and close it.

## Fix

All steps are **HUMAN-ONLY** Finance and deployer actions; the service is not changed and the
chain is not added to any route.

1. Finance recreates the admin and treasury Safe at its mainnet address on the other chain (same
   Safe proxy factory, singleton, initializer, and salt nonce used on mainnet), then adds the
   network (`WRONG_NETWORK`, its chain id) to `deploy/contracts/safe-expectations.json` in a
   reviewed PR and runs `deploy/contracts/verify-safe.sh` against two providers for that network.
2. The deployer deploys the canonical proxy and the factory with the unchanged constructor
   arguments, exactly as in `deploy/CONTRACTS.md`, and verifies that the forwarder address matches:

   ```sh
   deploy/contracts/deploy-proxy.sh --rpc-url "$WRONG_CHAIN_RPC_URL"
   deploy/contracts/deploy-factory.sh --rpc "$WRONG_NETWORK"/a="$WRONG_CHAIN_RPC_URL" --dry-run
   deploy/contracts/deploy-factory.sh --rpc "$WRONG_NETWORK"/a="$WRONG_CHAIN_RPC_URL" --broadcast
   cast call "$FACTORY" 'addressOf(address,bytes32)(address)' "$TREASURY" "$SALT" --rpc-url "$WRONG_CHAIN_RPC_URL"
   ```

   Stop if `addressOf` is not `DEPOSIT_ADDRESS` or the factory address differs from mainnet.
3. Anyone, usually the treasury Safe, calls the permissionless factory's
   `flush(treasury, [salt], token)` on that chain (`address(0)` for the native coin); the forwarder
   can only pay the treasury. Prepare the calldata for review:

   ```sh
   cast calldata 'flush(address,bytes32[],address)' "$TREASURY" "[$SALT]" "$WRONG_CHAIN_TOKEN"
   ```

4. Finance returns the recovered tokens, net of gas, from the treasury Safe on that chain to an
   address the user controls and confirms in writing (never the sender, which may be an exchange
   hot wallet), or agrees another settlement with the product. The service holds no deposit row for
   this payment, so the return is recorded in the support case and the finance ledger, not through
   `POST /v1/refunds`.

## Done when

The forwarder's token balance on the other chain is zero, the factory emitted `Flushed` for the
salt, the treasury Safe received the amount, and the return transaction to the user is final and
linked from the support case:

```sh
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

## Rollback

There is nothing to roll back in the service. A factory deployed on the other chain stays there
and is harmless: it has no roles or admin, and every forwarder can only pay its treasury. Do not add the chain to a route without the architecture review in §8.
