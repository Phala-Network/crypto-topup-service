# Rejected funds at treasury

**Trigger:** `TopupUnsupportedInflows` (tag `chain_id`, field `count`: finalized transfers of an
unrouted token to our addresses), Finance seeing treasury inflow tied to rejected deposits, or
rejected holdings that disagree with the flushes.

**Impact:** rejected deposits are never credited. Rejections of the route's token
(`below_minimum`, `out_of_bounds`, `out_of_range`, `sanctioned`, `product_refused`) are swept to
the treasury with everything else; an unsupported token stays in its forwarder, because the
flusher sweeps only routed tokens. The case may be a reporting question, a refundable customer
case, or a custody mismatch.

## First steps

1. Read the route's `rejected_holds_atomic` and `treasury_balance_atomic` in the daily report
   (`admin GET /v1/admin/report/daily`).
2. Find the deposits with a support lookup by address or `tx_hash`: state, reason, and timeline.
3. Check the chain:

   ```sh
   cast call "$TOKEN" 'balanceOf(address)(uint256)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"
   cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,logs}'
   ```

## Decide

- Expected rejection and a matching flush: custody is correct; classify refund eligibility
  (architecture §15).
- A route-token rejection with no matching flush: [flush reverted or bisected](flush-reverted-or-bisected.md).
- Treasury inflow differs from the `Flushed` events: critical reconciliation incident; pause
  `flush` and consider revoking the operator ([operator key compromise](operator-key-compromise.md)).
- Sanctioned funds: Compliance owns the disposition; no refund until it is recorded.

## Fix

Eligible deposits go through [refund execution](refund-execution.md). Returning an unsupported
token first needs a separately reviewed Safe flush of that token, as in
[wrong-network deposit](wrong-network-deposit.md) step 3.

## Done when

For every reviewed deposit, the chain amount, the flush, the treasury receipt, the reason, and the
refund disposition agree, and Finance signs off the case list.
