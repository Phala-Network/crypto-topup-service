# Stuck settlement exercise

Date: 2026-09-22.

Status: partial; seeded nudge is complete, but a processing/409 mock-product scenario is absent.

G2 exercised once: [ ]

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

In `admin_nudge_and_daily_report_use_seeded_integer_facts`, the seeded signed nudge returned HTTP
200, set `next_attempt_at` due without changing the terminal deposit state, and appended exactly one
`deposit_nudged` audit row. A product that remains `processing`/`409` was not exercised, so G2
remains unchecked.
