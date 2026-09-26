# Flush reverted or bisected

**Trigger:** the flusher alerts `Reverted` (`flush_id`, `nonce`), `IsolatedAddress` (`address`,
`salt`, `token`), `PlanningExcluded` (`reason`), or `FeeCapReached`; the `topup-flush-<route>`
monitor checking in `error` or missing; a `flush_planning.outcome` of `failed` or `send_failed` in
the daily report; or `TopupDepositStateAgeExceeded` with `state:credited`.

**Impact:** a reverted batch moves nothing (batches are atomic). The flusher retries with a fresh
nonce, then bisects; a persistently failing address is excluded while the rest continue. Treasury
exposure grows for the affected addresses. A credited deposit whose forwarder holds less than the
route's `min_flush_atomic` is never swept, by design.

## First steps

1. Read the alert's fields in Sentry and the route's `flush_planning` (`at`, `outcome`, `error`)
   and `unflushed_balance_atomic` in the daily report.
2. Read the transaction: find the operator's transaction with the alert's nonce on a block
   explorer, then

   ```sh
   cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,logs}'
   cast call "$TOKEN" 'balanceOf(address)(uint256)' "$FORWARDER_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
   ```

3. To stop new sends for the route: `admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["flush"]}'`.
   The sender re-checks the pause under the operator lock before every send; unsigned plans are
   voided and planned again after resume, and a broadcast transaction still confirms or is
   replaced. For a chain-wide stop that does not depend on the service, the admin Safe revokes
   `OPERATOR_ROLE` ([operator key compromise](operator-key-compromise.md)).

## Decide

- One revert: the flusher retries with a fresh nonce, then bisects; wait.
- Isolated address: check the token's behaviour and the forwarder's balance; only that address
  stays excluded.
- `FeeCapReached`: review the route's gas policy, not the batch.
- `MissingConsumedReceipt` or unknown operator transactions: [operator key compromise](operator-key-compromise.md).
- A pause during bisection voids the replan, so bisection restarts after resume; keep such a pause
  short.

## Fix

The flusher owns retries and bisection; there is no manual flush. Fix the token, RPC, or config
cause (a config change is a route upgrade), then resume:
`admin POST "/v1/admin/routes/$ROUTE/resume" '{"scopes":["flush"]}'`. If the role was revoked,
the Safe re-grants only a verified, uncompromised operator first.

## Done when

A new flush confirms with its `Flushed` logs, `flush_planning.outcome` is `planned` or `idle`,
`unflushed_balance_atomic` falls, and `topup-flush-<route>` checks in `ok`.
