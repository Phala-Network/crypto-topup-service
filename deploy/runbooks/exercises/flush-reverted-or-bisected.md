# Flush reverted or bisected exercise

Date: 2026-09-22.

Status: complete local integration scenario.

G2 exercised once: [x]

The test used a task-scoped PostgreSQL 16 container and spawned its own local Anvil process. Both
database URLs were set, so the test did not take its environment-variable skip path.

```sh
export PATH="$HOME/.cargo/bin:$HOME/.foundry/bin:$PATH"
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55435/postgres
cargo test --locked -p topup --features dev-signer --test flusher \
  anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect \
  -- --nocapture
```

Observed:

```text
running 1 test
test anvil_flush_lifecycle_covers_linkage_replacement_recovery_rotation_and_bisect ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out
```

The completed assertions deployed `SelectiveRevertingToken`, first excluded a singleton during gas
estimation, then sent an atomic batch that reverted after the token was changed, recorded the
reverted flush, retried with fresh plans, and bisected until `IsolatedAddress` identified the
blocked forwarder. The same scenario also verified C7 stale-plan operator re-binding and its
`flush.plan_operator_rebound` audit record.
