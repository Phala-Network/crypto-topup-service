# Wrong-network deposit

## Trigger

A user reports sending tokens to their deposit address on another EVM chain (for example BNB
Chain, Arbitrum, Base, or Polygon) instead of Ethereum mainnet, usually because an exchange
withdrawal defaulted to a cheaper network. The service watches only configured chains, so no
deposit row, event, or credit exists for the payment.

## Impact and blast radius

One user's funds sit at the same address on the other chain. Deposit addresses are CREATE2
forwarder clones of the factory, and the factory is deployed through the canonical deterministic
deployment proxy with identical init code on every chain (see `deploy/CONTRACTS.md`), so the same
factory, implementation, and forwarder addresses can be reproduced on the other chain. Nobody,
including the service, can move the funds until Finance deploys the factory there; recovery is
manual, costs gas on that chain, and is never automatic or guaranteed. Other users and chains are
unaffected. The architecture's customer copy (§12) warns "Ethereum mainnet only" on every address.

## First 5 minutes

Support collects, through the agreed support channel: the chain name and chain id, the transaction
hash, the token contract on that chain, the amount, and the workspace. Confirm the address belongs
to the workspace and read its salt with the application role:

```sh
export WRONG_CHAIN_RPC_URL=https://rpc.example-other-chain
export DEPOSIT_ADDRESS=0x...
export WRONG_CHAIN_TOKEN=0x...
export TX_HASH=0x...
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=address="$DEPOSIT_ADDRESS" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT ad.chain_id, ad.kind, ad.version, ad.lock_ref, ad.salt, ad.retired_at,
       a.external_id, p.slug AS product
FROM addresses ad
JOIN accounts a ON a.id = ad.account_id
JOIN products p ON p.id = a.product_id
WHERE lower(ad.address) = lower(:'address');
COMMIT;
SQL
cast chain-id --rpc-url "$WRONG_CHAIN_RPC_URL"
cast receipt "$TX_HASH" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast code "$FACTORY" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

Record the salt (`export SALT=0x...`), the chain id, the balance still at the address, and whether
the factory already has code on that chain. Tell the user that the payment was not credited, that recovery is a manual Finance
decision, and that there is no automatic refund; never promise an outcome or a date.

## Decision tree

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

## Remediation

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
   cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$WRONG_CHAIN_RPC_URL"
   ```

   Stop if `addressOf` is not `DEPOSIT_ADDRESS` or the factory address differs from mainnet.
3. The admin Safe (the factory's `DEFAULT_ADMIN_ROLE` holder) grants itself `OPERATOR_ROLE` on
   that chain's factory, calls `flush([salt], token)` (`address(0)` for the native coin), and
   revokes the role in the same Safe batch, so no service or hot key holds the role there. Prepare
   the calldata for review:

   ```sh
   export ADMIN_SAFE=0x...
   export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
   cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$ADMIN_SAFE"
   cast calldata 'flush(bytes32[],address)' "[$SALT]" "$WRONG_CHAIN_TOKEN"
   cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$ADMIN_SAFE"
   ```

4. Finance returns the recovered tokens, net of gas, from the treasury Safe on that chain to an
   address the user controls and confirms in writing (never the sender, which may be an exchange
   hot wallet), or agrees another settlement with the product. The service holds no deposit row for
   this payment, so the return is recorded in the support case and the finance ledger, not through
   `refund-requests`.

## Verification

The forwarder's token balance on the other chain is zero, the factory emitted `Flushed` for the
salt, the treasury Safe received the amount, the admin Safe no longer holds `OPERATOR_ROLE`, and
the return transaction to the user is final and linked from the support case:

```sh
cast call "$WRONG_CHAIN_TOKEN" 'balanceOf(address)(uint256)' "$DEPOSIT_ADDRESS" --rpc-url "$WRONG_CHAIN_RPC_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$ADMIN_SAFE" --rpc-url "$WRONG_CHAIN_RPC_URL"
```

## Rollback

There is nothing to roll back in the service. A factory deployed on the other chain stays there
and is harmless: only the admin Safe can grant `OPERATOR_ROLE`, and every forwarder can only pay the
treasury. Do not add the chain to a route without the architecture review in §8.
