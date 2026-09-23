# Reconciliation mismatch

## Trigger

Trigger on `TopupReconciliationMismatch` (PR #56: any increase of
`topup_reconciliation_mismatches_total{check=...}` in 15 minutes), a `reconciliation mismatch`
warning in the service log, `TopupLoopStopped{loop="reconciler"}`, or a non-zero exit from
`topup reconcile --once`.

## Impact and blast radius

C8 runs the architecture section 13 checks every `--reconciliation-interval-s` (default 600 s).
Each first observation is stored once in append-only `reconciliation_findings` with an `audit` row.
Findings with `repair_applied=true` are the safe repairs section 13 allows and do not increment the
metric. Mismatches act by check:

| `check_name` | Automatic action | Blast radius |
|---|---|---|
| `address_derivation` | `chain:<chain_id>` block; follow [Chain frozen](chain-frozen.md) | Whole chain |
| `credit_recomputation` | `address:<address_id>` block; the address is excluded from flush planning | One address |
| `custody_balance` | Alert only | Address or treasury totals |
| `missing_deposit` | Repair: insert `detected` | One deposit |
| `missing_flush_link` | Repair: link the deposit to its confirmed `flushed` row | One deposit |
| `sent_settlement` | Repair: adopt the product's GET answer under a lease | One deposit |
| `post_restore_settlement` | Restore gate stays incomplete | Every restored settlement |
| `lock_exposure` | Repair: recompute the `lock_exposure` counter under its row lock and write a `repair_lock_exposure` audit row; `TopupLockExposureDrift` fires for repairs by the service loop only (not `reconcile --once`); follow [Lock expiry worker failure](lock-expiry-worker-failure.md) | One exposure scope |

## First 5 minutes

Read the findings, blocks, and reconciler audit trail with the application role:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT check_name,subjects,expected,observed,repair_applied,incomplete,created_at
FROM reconciliation_findings ORDER BY created_at DESC, check_name LIMIT 50;
SELECT block_key,scope,chain_id,address_id,check_name,reason,created_at
FROM reconciliation_blocks ORDER BY created_at, block_key;
SELECT action,subject,reason,created_at FROM audit
WHERE actor='reconciler' ORDER BY created_at DESC LIMIT 20;
COMMIT;
SQL
```

For a custody finding, compare the stored expectation with both providers at the finalized block:

```sh
export FINALIZED_BLOCK="$(cast block finalized -f number --rpc-url "$RPC_PROVIDER_A_URL")"
cast call "$TOKEN" 'balanceOf(address)(uint256)' "$FORWARDER_ADDRESS" --block "$FINALIZED_BLOCK" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$TOKEN" 'balanceOf(address)(uint256)' "$FORWARDER_ADDRESS" --block "$FINALIZED_BLOCK" --rpc-url "$RPC_PROVIDER_B_URL"
```

To re-run the checks on demand after a fix, run one pass inside the service container. It uses the
same application role and attested route files, and is idempotent:

```sh
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup reconcile --once --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml
```

## Decision tree

- `address_derivation`: follow [Chain frozen](chain-frozen.md).
- `credit_recomputation`: compare the stored price, valuation time, amount, and route version with
  the product's accepted payload. If the product credited a different amount, open a Finance
  incident; do not change `credit_minor`.
- `custody_balance` on one address: check for an unrecorded transfer, a pending flush, or a
  fee-on-transfer token. On treasury totals: compare treasury inflow with `Flushed` events and
  involve Finance.
- Repairs only (`repair_applied=true`): verify the repaired rows advance; no manual action.
- The reconciler reports failed checks rather than findings: check both providers first.

## Remediation

Fix the cause, not the finding. Findings and audit rows are append-only evidence. The application
role cannot delete blocks; lifting one is **HUMAN-ONLY** for the database owner after Engineering
and Finance sign off, from an owner session outside the service container. Never place owner
credentials in the service container:

```sql
DELETE FROM reconciliation_blocks WHERE block_key = 'address:<address_id>';
```

If the mismatch persists, the next pass writes the block again.

## Verification

A fresh `topup reconcile --once` exits `0`, no new mismatch appears for the subject, the block is
absent, and flush planning includes the address again. `TopupReconciliationMismatch` resolves
after its 15-minute window.

## Rollback

Nothing to roll back in the service: blocks are re-created automatically while a mismatch persists.
If a lifted block was premature, the database owner does not need to re-insert it; the next pass
does. Keep the finding and audit rows.
