# Wrong-network deposit

**Trigger:** a merchant reports that its customer sent tokens to their deposit address on an EVM
chain no route covers (for example BNB Chain, Arbitrum, Base, or Polygon) instead of the route's
chain, usually because an exchange withdrawal defaulted to a cheaper network. The service watches
only configured chains, so no deposit row, event, or credit exists for the payment.

**Impact:** one customer's funds sit at the same address on the other chain. Deposit addresses are
CREATE2 forwarder clones of the factory, and the factory is deployed through the canonical
deterministic deployment proxy with identical init code on every chain (see `deploy/CONTRACTS.md`),
so the same factory, implementation, and forwarder addresses can be reproduced on the other chain,
and a forwarder there pays only the treasury its address commits to. Nobody, including the service,
can move the funds until the factory is deployed there; recovery is manual, costs gas on that
chain, and is never automatic or guaranteed. The funds are the merchant's to recover and return:
the service holds no key and sends no transaction. Other customers and chains are unaffected. The
architecture's customer copy (§12) warns about the network on every address.

## First steps

The merchant collects from its customer: the chain name and id, the transaction hash, the token
contract on that chain, and the amount. It confirms that the address is one of its forwarders and
reads its `salt` and `treasury` (`GET /v1/forwarders`, or the deposit address's network). Check
that the configured factory derives the address from them, then read the other chain:

```sh
export WRONG_CHAIN_RPC_URL=https://rpc.example-other-chain
export DEPOSIT_ADDRESS=0x... SALT=0x... WRONG_CHAIN_TOKEN=0x... TX_HASH=0x...
cast call "$FACTORY" 'addressOf(address,bytes32)(address)' "$TREASURY" "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
cast chain-id --rpc-url "$WRONG_CHAIN_RPC_URL"
cast receipt "$TX_HASH" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast code "$FACTORY" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast code "$TREASURY" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

Record the chain id, the balance still at the address, and whether the factory and the treasury
already have code there. The merchant tells its customer the payment was not credited, that
recovery is a manual decision, and that there is no automatic refund; never promise an outcome or
a date.

## Decide

- `addressOf` is not `DEPOSIT_ADDRESS`: stop; the funds are not at one of the account's
  forwarders.
- Balance is zero: the funds already moved or never arrived there; ask for the transaction on an
  explorer and stop.
- The chain does not use standard CREATE2 addresses (for example zkSync Era) or rejects the
  deterministic deployment proxy's pre-EIP-155 transaction (`deploy/CONTRACTS.md`, Canonical
  proxy): the address cannot be reproduced with this factory; escalate to Engineering. Recovery
  may be impossible.
- The merchant cannot control its treasury address on that chain: an EOA treasury's key works on
  every EVM chain, but a Safe treasury exists there only if its owners can create it at the same
  address (same Safe proxy factory, singleton, initializer, and salt nonce). Otherwise the
  forwarder could still be deployed, but it would flush to an address nobody controls there. Do
  not flush.
- Recoverable, and the merchant decides the amount justifies the gas: continue with Fix. Below
  that threshold, the merchant records the decision and closes the case.

## Fix

All steps are **HUMAN-ONLY**; the service is not changed and the chain is not added to any route.

1. For a Safe treasury, its owners recreate it at the same address on the other chain.
2. A deployer (the merchant, or the operator's deployer on request; the deployment is permissionless)
   deploys the canonical proxy and the factory exactly as in `deploy/CONTRACTS.md` and verifies
   that the forwarder address matches:

   ```sh
   deploy/contracts/deploy-proxy.sh --rpc-url "$WRONG_CHAIN_RPC_URL"
   deploy/contracts/deploy-factory.sh --rpc "$WRONG_NETWORK"/a="$WRONG_CHAIN_RPC_URL" --dry-run
   deploy/contracts/deploy-factory.sh --rpc "$WRONG_NETWORK"/a="$WRONG_CHAIN_RPC_URL" --broadcast
   cast call "$FACTORY" 'addressOf(address,bytes32)(address)' "$TREASURY" "$SALT" --rpc-url "$WRONG_CHAIN_RPC_URL"
   ```

   `WRONG_NETWORK` must be a network of `deploy/contracts/networks.json` (add it with its chain id
   in a reviewed PR). Stop if `addressOf` is not `DEPOSIT_ADDRESS` or the factory address differs
   from the route's.
3. Anyone, usually the merchant's treasury, calls the permissionless factory's
   `flush(treasury, [salt], token)` on that chain (`address(0)` for the native coin); the forwarder
   can only pay the treasury. Prepare the calldata for review:

   ```sh
   cast calldata 'flush(address,bytes32[],address)' "$TREASURY" "[$SALT]" "$WRONG_CHAIN_TOKEN"
   ```

4. The merchant returns the recovered tokens, net of gas, from its treasury on that chain to an
   address its customer controls and confirms in writing (never the sender, which may be an
   exchange hot wallet), or settles otherwise with its customer. The service holds no deposit row
   for this payment, so the return is recorded in the merchant's own records, not through
   `POST /v1/refunds`.

## Done when

The forwarder's token balance on the other chain is zero, the factory emitted `Flushed` for the
salt, the treasury received the amount, and the merchant confirms the return to its customer:

```sh
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

## Rollback

There is nothing to roll back in the service. A factory deployed on the other chain stays there
and is harmless: it has no roles or admin, and every forwarder can only pay its treasury. Do not
add the chain to a route without the architecture review in §8.
