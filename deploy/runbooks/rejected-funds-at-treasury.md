# Rejected funds at treasury

## Trigger

Trigger when Finance sees treasury inflow tied to `rejected` deposits, on `TopupUnsupportedInflows`
(PR #56: finalized inflows of an unsupported asset), or when rejected holdings reported by custody
records differ from finalized `Flushed` events.

## Impact and blast radius

Rejected funds are intentionally not credited but still flush to treasury. The incident may be a
reporting misunderstanding, a refundable customer case, or a custody reconciliation mismatch.

## First 5 minutes

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.reason,d.asset_contract,d.amount_atomic::text,d.flush_id,f.tx_hash,f.block_number FROM deposits d LEFT JOIN flushes f ON f.id=d.flush_id WHERE d.state='rejected' ORDER BY d.updated_at DESC LIMIT 100; COMMIT;"
cast call "$TOKEN" 'balanceOf(address)(uint256)' "$TREASURY" --rpc-url "$RPC_PROVIDER_A_URL"
cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,logs}'
```

## Decision tree

- Expected below-minimum/unsupported/rejected funds and matching flush: custody is correct; classify
  refund eligibility.
- Deposit has no matching flush: follow flush/reconciliation investigation.
- Treasury event differs from stored `Flushed`: critical reconciliation incident; use the `flush`
  pause and optional Safe role revocation procedure in
  [Flush reverted or bisected](flush-reverted-or-bisected.md), including its #71 caveat.
- Sanctioned funds: Compliance owns disposition; do not refund automatically.

## Remediation

For eligible deposits, follow [refund execution](refund-execution.md). Use the signed daily report
for route-level `rejected_holds_atomic` and refund-status reconciliation; do not replace it with an
ad hoc write or spreadsheet mutation:

```sh
: > /tmp/empty
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh GET "$BASE_URL/v1/admin/report/daily" /tmp/empty "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X GET -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" "$BASE_URL/v1/admin/report/daily"
```

## Verification

For every reviewed deposit, chain amount, `flushed.amount_atomic`, treasury receipt, rejection
reason, and refund disposition agree. Finance signs off the case list.

## Rollback

There is no rollback for finalized treasury inflow. Pause `flush` and revoke the operator role if
reconciliation regresses; use forward accounting/refund actions only.
