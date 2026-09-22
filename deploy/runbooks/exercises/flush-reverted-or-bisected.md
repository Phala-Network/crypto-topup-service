# Flush reverted or bisected exercise

Date: 2026-09-22.

Status: complete local integration scenario, re-run after merging C7b, C8, and C10.

G2 exercised once: [x]

The test used the task-scoped PostgreSQL 16 container from [local setup](local-setup.md) and spawned
its own Anvil process. Both database URLs were set, so the test did not take its skip path.

```sh
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
cargo test --locked -p topup --features dev-signer --test flusher -- --nocapture
```

Observed after merging C7b (#70):

```text
test timed_out_rpc_does_not_hold_the_operator_lock ... ok
test planner_skips_frozen_chains_and_blocked_addresses ... ok
test flush_pauses_gate_planning_and_sending_without_blocking_confirmation ... ok
test anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 40.92s
```

`anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect` deployed
`SelectiveRevertingToken`, first excluded a singleton during gas estimation, then sent an atomic
batch that reverted after the token was changed, recorded the reverted flush, retried with fresh
plans, and bisected until `IsolatedAddress` identified the blocked forwarder. It also verified C7
stale-plan operator re-binding and its `flush.plan_operator_rebound` audit record.
`flush_pauses_gate_planning_and_sending_without_blocking_confirmation` covers the runbook's stop:
account, product, and route `flush` pauses exclude addresses from planning, a plan paused before
send stays `planned` with one `flush.send_paused` audit row, and an already sent flush still
confirms.

The runbook's `sent`-id snapshot comparison and its pause/audit query ran as `wp_d5_app` against the
migrated schema (see [local setup](local-setup.md#runbook-sql)). The Finance Safe `OPERATOR_ROLE`
revocation is human-only; its commands were rehearsed on Anvil in the
[operator key compromise exercise](operator-key-compromise.md).
