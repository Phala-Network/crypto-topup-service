# Operator gas refill

## Trigger

Trigger on `TopupOperatorGasReserveLow` (PR #56: operator balance below 0.001 native token for
five minutes) or when the balance cannot cover a bounded flush at the configured fee cap.

## Impact and blast radius

Flushes stop for every route using the operator on that chain. Credits and custody attribution can
continue, but treasury exposure and unflushed balances grow.

## First 5 minutes

```sh
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
cast nonce "$OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" \
  --set=operator_address="$OPERATOR_ADDRESS" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,token,nonce::text,status,tx_hash,receipt FROM flushes
WHERE chain_id=:chain_id AND operator=lower(:'operator_address') ORDER BY nonce DESC LIMIT 20;
COMMIT;
SQL
```

If the balance cannot fund the next bounded attempt, pause the `flush` scope on every route of the
chain with the signed request in [Flush reverted or bisected](flush-reverted-or-bisected.md) to
prevent fee-estimation/send churn, and run its no-new-`sent` check. A paused plan at the lowest
nonce holds every later flush on the chain
([#71](https://github.com/Phala-Network/crypto-topup-service/issues/71)), which is acceptable here
because the operator cannot pay for any of them. Revoke `OPERATOR_ROLE` through the Finance Safe
only if unexpected spend points to a compromise.

## Decision tree

- Balance low and no unexpected spend: refill to the approved target.
- Balance low with unknown transactions: follow operator key compromise first.
- Fee cap reached but balance healthy: review gas policy; do not refill as a substitute.

## Remediation

**HUMAN-ONLY, Finance Safe:** submit a native-token Safe transfer to `$OPERATOR_ADDRESS` for the
approved amount. Record the Safe transaction hash and do not send from a personal key.

```sh
cast receipt "$SAFE_TRANSACTION_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,transactionHash}'
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

## Verification

Both providers show the finalized balance and the pending nonce is expected. Resume the `flush`
scope, then require flush maintenance to confirm or replace the existing row and send any plan that
logged `flush.send_paused`.

## Rollback

A finalized refill cannot be rolled back. If sent to the wrong address, pause `flush`, revoke the
operator role if required, open a Finance incident, and rotate the operator if key ownership is
uncertain.
