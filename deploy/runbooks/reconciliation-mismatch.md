# Reconciliation mismatch

**Trigger:** `TopupReconciliationMismatch` (tagged with its `check`), or the `topup-reconciler`
Crons monitor checking in `error` (a check could not complete) or missing its check-in.

**Impact:** the reconciler runs the architecture §13 checks every 10 minutes (when `finalized` has
advanced since the last round) and stores each first
observation once. Safe repairs raise no alert. By check:

| `check` | Automatic action | Blast radius |
|---|---|---|
| `address_derivation` | freezes the chain: [Chain frozen](chain-frozen.md) | whole chain |
| `custody_balance` | freezes the chain: [Chain frozen](chain-frozen.md) | whole chain |
| `credit_recomputation` | alert only | one deposit |
| `missing_deposit`, `missing_flush_link`, `sent_settlement` | repair: insert, link, or adopt the product's answer | one deposit |
| `post_restore_settlement` | keeps a restore check incomplete ([RESTORE.md](../RESTORE.md)) | the restore |

## First steps

1. Read the finding's `subjects`, `expected`, and `observed` from the Sentry event.
2. For `error` check-ins, read `reconciliation.failed_checks` in the daily report
   (`admin GET /v1/admin/reports/daily`); a failed check is usually an RPC error: check both
   providers first.

## Decide

- `credit_recomputation`: compare the deposit's stored valuation (support lookup) with the
  product's accepted payload. If the product credited a different amount, open a Finance incident;
  never change the credit.

## Fix

Fix the cause, not the finding. A chain freeze is lifted as [Chain frozen](chain-frozen.md)
describes; the lift does not re-check, so while the mismatch persists every round freezes again.

## Done when

The next round raises no new finding for the subject and `topup-reconciler` checks in `ok`.
