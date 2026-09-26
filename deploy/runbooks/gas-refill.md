# Operator gas refill

**Trigger:** `TopupOperatorGasReserveLow` (tags `chain`, `route`; fields `operator`,
`balance_wei`, `reserve_wei`): on every maintenance tick while it holds `OPERATOR_ROLE`, the
flusher found the operator's native balance below the route's
`chain.flush.min_operator_balance_wei`. Also a `send_failed` flush planning outcome for
insufficient funds.

**Impact:** flushes stop for every route on the chain that uses the operator. Credits continue;
unflushed balances and treasury exposure grow.

## First steps

```sh
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
cast nonce "$OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
```

Compare the spend with the operator's transactions on a block explorer. If the balance cannot fund
the next attempt, pause `flush` on every route of the chain
([flush reverted or bisected](flush-reverted-or-bisected.md), step 3).

## Decide

- Low balance, all spend explained by flushes: refill to the approved target.
- Unknown transactions: [operator key compromise](operator-key-compromise.md) first.
- `FeeCapReached` with a healthy balance: review the gas policy; do not refill instead.

## Fix

**HUMAN-ONLY, Finance Safe:** a native-coin Safe transfer of the approved amount to
`$OPERATOR_ADDRESS`; never from a personal key. Record the Safe transaction hash.

## Done when

Both providers show the new balance at `finalized`, `flush` is resumed, and the next flush
confirms. A refill sent to the wrong address cannot be undone: pause `flush` and open a Finance
incident.
