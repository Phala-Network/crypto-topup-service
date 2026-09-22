# Flush reverted or bisected exercise

Date: 2026-09-22.

Status: complete local integration scenario, re-run after merging C8 and C10.

G2 exercised once: [x]

The test used the task-scoped PostgreSQL 16 container from [local setup](local-setup.md) and spawned
its own Anvil process. Both database URLs were set, so the test did not take its skip path.

```sh
export PATH="$HOME/.cargo/bin:$HOME/.foundry/bin:$PATH"
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
cargo test --locked -p topup --features dev-signer --test flusher \
  anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect \
  -- --nocapture
```

Observed:

```text
running 1 test
test anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 8.95s
```

The completed assertions deployed `SelectiveRevertingToken`, first excluded a singleton during gas
estimation, then sent an atomic batch that reverted after the token was changed, recorded the
reverted flush, retried with fresh plans, and bisected until `IsolatedAddress` identified the
blocked forwarder. The same scenario also verified C7 stale-plan operator re-binding and its
`flush.plan_operator_rebound` audit record.

The runbook's manual stop check (`sent` flush ids captured before and after stopping the service,
compared with `comm`) was run as `wp_d5_app` against the reconciliation exercise database and
returned an empty difference. Stopping a staging service and the Finance Safe `OPERATOR_ROLE`
revocation are human-only and were not exercised; the `flush` pause scope is not a stop until
[#61](https://github.com/Phala-Network/crypto-topup-service/issues/61) lands.
