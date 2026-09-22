# 422 payload mismatch exercise

Date: 2026-09-22.

Status: complete local PostgreSQL/mock-product scenario, re-run after merging C8 and C10.

G2 exercised once: [x]

The tests used the task-scoped PostgreSQL 16 container from [local setup](local-setup.md). Both
database URLs were set, so the tests created disposable databases and application roles rather than
taking the skip path.

```sh
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55436/postgres
cargo test --locked -p topup --test settle payload_mismatch -- --nocapture
cargo test --locked -p topup --test settle \
  sent_row_gets_authoritative_answer_and_adopts_product_pricing -- --nocapture
```

Observed:

```text
test payload_mismatch_is_alert_retry_and_is_never_resent ... ok
test payload_mismatch_guard_survives_unknown_and_missing_gets ... ok
test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 7 filtered out; finished in 2.06s
test sent_row_gets_authoritative_answer_and_adopts_product_pricing ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out; finished in 1.08s
```

The first scenario asserted one POST received `422`, persisted `resend_forbidden=true`, emitted
alert-level transition evidence, performed GET on retry, and never issued a second POST. The second
kept the resend guard when the later GET answered unknown or failed. The third seeded a sent row and
asserted GET-first adoption of the mock product's authoritative amount, price, valuation time,
destination transaction ID, transition evidence, and outbox payload. The runbook's read-only
diagnostic query also ran as `wp_d5_app` against the migrated schema (see
[local setup](local-setup.md#runbook-sql)).
