# Rejected funds at treasury

**Trigger:** `TopupUnsupportedInflows` (tag `chain_id`, field `count`: finalized transfers of an
unrouted token to our addresses), Finance seeing treasury inflow tied to rejected deposits, or
rejected holdings that disagree with the sweeps.

**Impact:** rejected deposits are never credited. Rejections of the route's token
(`below_minimum`, `out_of_bounds`, `out_of_range`, `sanctioned`, `product_refused`) reach the
treasury with everything else when the merchant sweeps the forwarder; an unsupported token stays
in its forwarder until someone flushes that token. The case may be a reporting question, a
refundable customer case, or a custody mismatch.

## First steps

1. Read the route's `rejected_holds_atomic` in the daily report
   (`admin GET /v1/admin/report/daily`); `$TREASURY` below is the treasury the deposit's address
   pays (the support lookup's address row), which may differ from the account's current one.
2. Find the deposits with a support lookup by address or `tx_hash`: state, reason, and timeline.
3. Check the chain:

   ```sh
   cast call "$TOKEN" 'balanceOf(address)(uint256)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"
   cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,logs}'
   ```

## Decide

- Expected rejection and a matching flush: custody is correct; classify refund eligibility
  (architecture §15).
- A route-token rejection with no matching flush: the forwarder has not been swept yet, or its
  flush failed (`FlushFailed`); the merchant sweeps ([deploy/README.md, "Sweeping"](../README.md#sweeping)).
- Treasury inflow differs from the `Flushed` events: critical reconciliation incident; see
  [chain frozen](chain-frozen.md) for `custody_balance`.
- Sanctioned funds: Compliance owns the disposition; no refund until it is recorded.

## Fix

The merchant refunds eligible deposits itself: it creates the refund, pays it from the treasury
of the deposit's address, and attaches the transaction with `mark_paid`
([integration guide, §3](../../docs/integration.md#3-refunds)). Returning an unsupported
token first needs a flush of that token (the factory's `flush(treasury, salts, token)`, which
anyone may call), as in [wrong-network deposit](wrong-network-deposit.md) step 3.

## Done when

For every reviewed deposit, the chain amount, the flush, the treasury receipt, the reason, and the
refund disposition agree, and Finance signs off the case list.
