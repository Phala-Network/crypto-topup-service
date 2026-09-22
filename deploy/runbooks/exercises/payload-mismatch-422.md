# 422 payload mismatch exercise

Date: 2026-09-22.

Status: complete local PostgreSQL/mock-product scenario.

G2 exercised once: [x]

The tests used a task-scoped PostgreSQL 16 container. `MIGRATE_DATABASE_URL` and `DATABASE_URL`
were set, so the tests created disposable databases and app roles rather than taking the skip path.

```sh
export SQLX_OFFLINE=true
export MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55435/postgres
export DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55435/postgres
cargo test --locked -p topup --test settle \
  payload_mismatch_is_alert_retry_and_is_never_resent -- --nocapture
cargo test --locked -p topup --test settle \
  sent_row_gets_authoritative_answer_and_adopts_product_pricing -- --nocapture
```

Observed:

```text
test payload_mismatch_is_alert_retry_and_is_never_resent ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out
test sent_row_gets_authoritative_answer_and_adopts_product_pricing ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 8 filtered out
```

The first scenario asserted one POST received `422`, persisted `resend_forbidden=true`, emitted
alert-level transition evidence, performed GET on retry, and never issued a second POST. The second
seeded a sent row and asserted GET-first adoption of the mock product's authoritative amount, price,
valuation time, destination transaction ID, transition evidence, and outbox payload.
