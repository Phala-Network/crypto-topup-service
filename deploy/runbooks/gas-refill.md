# Operator gas refill

## Trigger

Trigger when the operator native balance falls below the configured gas reserve or cannot cover a
bounded flush at the configured fee cap. PR #56 alert names are not on `main`.

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

If the balance cannot fund the next bounded attempt, pause flush using the signed admin pattern with
body `{"scopes":["flush"]}`.

## Decision tree

- Balance low and no unexpected spend: refill to the approved target.
- Balance low with unknown transactions: follow operator key compromise first.
- Fee cap reached but balance healthy: review gas policy; do not refill as a substitute.

## Remediation

**HUMAN-ONLY, Finance Safe:** submit a native-token Safe transfer to `$OPERATOR_ADDRESS` for the
approved amount. Record the Safe transaction hash and do not send from a personal key.

```sh
cast receipt "$SAFE_TRANSACTION_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '{status,blockNumber,transactionHash}'
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

## Verification

Both providers show the finalized balance, pending nonce is expected, and flush maintenance confirms
or replaces the existing row. Resume only `flush` if it was paused.

## Rollback

A finalized refill cannot be rolled back. If sent to the wrong address, keep flush paused, open a
Finance incident, and rotate the operator if key ownership is uncertain.
