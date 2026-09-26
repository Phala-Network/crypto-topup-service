# Reconciliation mismatch

**Trigger:** `TopupReconciliationMismatch` (tagged with its `check`), or the `topup-reconciler`
Crons monitor checking in `error` (a check could not complete) or missing its check-in.

**Impact:** the reconciler runs the architecture §13 checks every 10 minutes and stores each first
observation once. Safe repairs raise no alert. By check:

| `check` | Automatic action | Blast radius |
|---|---|---|
| `address_derivation` | freezes the chain: [Chain frozen](chain-frozen.md) | whole chain |
| `credit_recomputation` | blocks the address, which leaves flush planning | one address |
| `custody_balance` | alert only | an address or the treasury totals |
| `missing_deposit`, `missing_flush_link`, `sent_settlement` | repair: insert, link, or adopt the product's answer | one deposit |
| `post_restore_settlement` | keeps a restore check incomplete ([RESTORE.md](../RESTORE.md)) | the restore |

## First steps

1. Read the finding's `subjects`, `expected`, and `observed` from the Sentry event.
2. For `error` check-ins, read `reconciliation.failed_checks` in the daily report
   (`admin GET /v1/admin/report/daily`); a failed check is usually an RPC error: check both
   providers first.
3. For `custody_balance`, compare both providers at the finalized block:

   ```sh
   export FINALIZED_BLOCK="$(cast block finalized -f number --rpc-url "$RPC_PROVIDER_A_URL")"
   cast call "$TOKEN" 'balanceOf(address)(uint256)' "$FORWARDER_ADDRESS" --block "$FINALIZED_BLOCK" --rpc-url "$RPC_PROVIDER_A_URL"
   cast call "$TOKEN" 'balanceOf(address)(uint256)' "$FORWARDER_ADDRESS" --block "$FINALIZED_BLOCK" --rpc-url "$RPC_PROVIDER_B_URL"
   ```

## Decide

- `credit_recomputation`: compare the deposit's stored valuation (support lookup) with the
  product's accepted payload. If the product credited a different amount, open a Finance incident;
  never change the credit.
- `custody_balance` on one address: look for an unrecorded transfer, a pending flush, or a
  fee-on-transfer token. On treasury totals: compare treasury inflow with `Flushed` events and
  involve Finance.

## Fix

Fix the cause, not the finding. An address block can be lifted only by the database owner, which a
production CVM does not offer; escalate to Engineering. While the mismatch persists, every round
blocks again.

## Done when

The next round raises no new finding for the subject and `topup-reconciler` checks in `ok`.
