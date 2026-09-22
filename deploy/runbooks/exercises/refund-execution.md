# Refund execution exercise

Date: 2026-09-22.

Status: complete local PostgreSQL/API/confirmation-worker scenario.

G2 exercised once: [x]

The suite used the task-scoped PostgreSQL 16 container from [local setup](local-setup.md) with both
database URLs set, so it created disposable migrated databases and application roles rather than
taking the skip path.

```sh
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
cargo test --locked -p topup --test refunds -- --nocapture
```

Observed:

```text
test evm_reader_rejects_wrong_or_unfinalized_transfers_and_times_out ... ok
test unsupported_refund_approval_uses_persisted_fallback_route_pause ... ok
test corrected_hash_rejects_stale_observation_then_confirms_replacement ... ok
test one_transfer_log_confirms_only_one_refund ... ok
test refund_flow_confirms_only_matching_finalized_transfer ... ok
test refund_request_requires_rejection_and_approval_rechecks_current_state ... ok
test worker_shutdown_cancels_a_hung_observation ... ok
test support_lookup_uses_tenant_scoped_keyset_pages ... ok
test admin_nudge_and_daily_report_use_seeded_integer_facts ... ok
test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 17.34s
```

`refund_flow_confirms_only_matching_finalized_transfer` covered idempotent request creation, pause enforcement, approval
re-validation, signed admin approve and record calls, a finalized transfer amount mismatch that
left the row `sent`, a subsequent exact match that moved it to `confirmed`, one
`deposit.refunded` outbox event, and support lookup of the rejected deposit. Safe submission remains
**HUMAN-ONLY** in the runbook; the local confirmation path used the C12 scripted chain reader.
