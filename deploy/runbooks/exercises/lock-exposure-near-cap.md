# Lock exposure near cap exercise

Date: 2026-09-22.

Status: complete local PostgreSQL scenario for the runbook query and C10 cap enforcement (see
[local setup](local-setup.md), database `wp_d5_locks`). `TopupLockExposureNearCap` itself is
pending PR #56 and was not evaluated.

G2 exercised once: [x]

Seeded two products, three accounts, and five open reserved locks, one of them 20 minutes past
`expires_at`, with a `lock_exposure` ledger consistent with those reservations. Account
`acct-a` holds 450000 of its 500000 cap, exactly the 90% alert threshold:

```sh
docker exec -i wp-d5-exercise-pg psql -U postgres -d wp_d5_locks -v ON_ERROR_STOP=1 -q \
  < /tmp/wp-d5-ex/seed-locks.sql
```

```sql
INSERT INTO products (id,slug,settlement_url,webhook_url,pubkey,kid) VALUES
 ('10000000-0000-0000-0000-000000000001','mock-a','http://127.0.0.1:9/settle','http://127.0.0.1:9/hook','mock','mock/v1'),
 ('10000000-0000-0000-0000-000000000002','mock-b','http://127.0.0.1:9/settle','http://127.0.0.1:9/hook','mock','mock/v1');
INSERT INTO accounts (id,product_id,external_id) VALUES
 ('20000000-0000-0000-0000-000000000001','10000000-0000-0000-0000-000000000001','acct-a'),
 ('20000000-0000-0000-0000-000000000002','10000000-0000-0000-0000-000000000001','acct-b'),
 ('20000000-0000-0000-0000-000000000003','10000000-0000-0000-0000-000000000002','acct-c');
INSERT INTO addresses (id,account_id,chain_id,kind,version,lock_ref,salt,address) VALUES
 ('30000000-0000-0000-0000-000000000001','20000000-0000-0000-0000-000000000001',31337,'lock',1,'lock-a1','0x0000000000000000000000000000000000000000000000000000000000000001','0x0000000000000000000000000000000000000001'),
 ('30000000-0000-0000-0000-000000000002','20000000-0000-0000-0000-000000000001',31337,'lock',1,'lock-a2','0x0000000000000000000000000000000000000000000000000000000000000002','0x0000000000000000000000000000000000000002'),
 ('30000000-0000-0000-0000-000000000003','20000000-0000-0000-0000-000000000002',31337,'lock',1,'lock-b1','0x0000000000000000000000000000000000000000000000000000000000000003','0x0000000000000000000000000000000000000003'),
 ('30000000-0000-0000-0000-000000000004','20000000-0000-0000-0000-000000000002',31337,'lock',1,'lock-b2','0x0000000000000000000000000000000000000000000000000000000000000004','0x0000000000000000000000000000000000000004'),
 ('30000000-0000-0000-0000-000000000005','20000000-0000-0000-0000-000000000003',31337,'lock',1,'lock-c1','0x0000000000000000000000000000000000000000000000000000000000000005','0x0000000000000000000000000000000000000005');
INSERT INTO rate_locks (address_id,route,amount_atomic,price_scaled,credit_minor,expires_at,status,exposure_reserved) VALUES
 ('30000000-0000-0000-0000-000000000001','local-anvil-pha-usd',1200000000000000000000,100000000,120000,now()+interval '15 minutes','open',true),
 ('30000000-0000-0000-0000-000000000002','local-anvil-pha-usd',3300000000000000000000,100000000,330000,now()+interval '10 minutes','open',true),
 ('30000000-0000-0000-0000-000000000003','local-anvil-pha-usd',1000000000000000000000,100000000,100000,now()+interval '12 minutes','open',true),
 ('30000000-0000-0000-0000-000000000004','local-anvil-pha-usd',500000000000000000000,100000000,50000,now()-interval '20 minutes','open',true),
 ('30000000-0000-0000-0000-000000000005','local-anvil-pha-usd',4000000000000000000000,100000000,400000,now()+interval '14 minutes','open',true);
INSERT INTO lock_exposure (scope_key,open_minor) VALUES
 ('account:20000000-0000-0000-0000-000000000001',450000),
 ('account:20000000-0000-0000-0000-000000000002',150000),
 ('account:20000000-0000-0000-0000-000000000003',400000),
 ('product:10000000-0000-0000-0000-000000000001',600000),
 ('product:10000000-0000-0000-0000-000000000002',400000),
 ('global',1000000);
```

Caps from the local route, then the runbook query as `wp_d5_app`:

```sh
grep -E '^[[:space:]]+max_open_minor:' /tmp/wp-d5-ex/route.yaml
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=account_cap=500000 \
  --set=product_cap=5000000 --set=global_cap=10000000 < /tmp/wp-d5-ex/lock-query-runbook.sql
```

```text
  max_open_minor: { account: 500000, product: 5000000, global: 10000000 }
BEGIN
                  scope_key                   | ledger_open_minor | open_reserved_minor | unexpired_minor | overdue_locks | cap_minor | ledger_bps_of_cap
----------------------------------------------+-------------------+---------------------+-----------------+---------------+-----------+-------------------
 account:20000000-0000-0000-0000-000000000001 | 450000            | 450000              | 450000          |             0 | 500000    |              9000
 account:20000000-0000-0000-0000-000000000003 | 400000            | 400000              | 400000          |             0 | 500000    |              8000
 account:20000000-0000-0000-0000-000000000002 | 150000            | 150000              | 100000          |             1 | 500000    |              3000
 product:10000000-0000-0000-0000-000000000001 | 600000            | 600000              | 550000          |             1 | 5000000   |              1200
 global                                       | 1000000           | 1000000             | 950000          |             1 | 10000000  |              1000
 product:10000000-0000-0000-0000-000000000002 | 400000            | 400000              | 400000          |             0 | 5000000   |               800
(6 rows)

COMMIT
```

The query ranks `acct-a` first at 9000 basis points, shows the ledger equal to the recomputation
for every scope, and separates the overdue lock (`overdue_locks=1`, excluded from
`unexpired_minor`) for the [lock expiry worker failure](lock-expiry-worker-failure.md) branch.

To exercise the ledger-mismatch branch, the owner then moved one counter away from the
reservations; the same query exposed the difference:

```text
UPDATE lock_exposure SET open_minor=390000 WHERE scope_key='product:10000000-0000-0000-0000-000000000002' -> UPDATE 1
                  scope_key                   | ledger_open_minor | open_reserved_minor | unexpired_minor | overdue_locks | cap_minor | ledger_bps_of_cap
 product:10000000-0000-0000-0000-000000000002 | 390000            | 400000              | 400000          |             0 | 5000000   |               780
```

C10 enforcement of each cap, release on expiry, and concurrent reservations were exercised by the
integration suite against the same PostgreSQL instance. The cap test asserts
`RateLockError::ExposureCap` for the account, product, and global scopes, which the API maps to
`409 exposure_cap_exceeded`:

```sh
cargo test --locked -p topup --test rate_locks -- --nocapture
```

```text
test concurrent_creations_never_exceed_the_shared_exposure_cap ... ok
test account_product_global_caps_and_expiry_release_are_atomic ... ok
test rate_limited_creation_does_not_fetch_a_price ... ok
test new_lock_addresses_start_scanning_at_the_chain_cursor ... ok
test api_is_idempotent_rate_limited_paused_tenant_safe_and_emits_eip681 ... ok
test cancel_refuses_a_lock_whose_address_received_any_deposit ... ok
test expiring_two_accounts_does_not_deadlock_with_a_concurrent_creation ... ok
test usd_stated_amount_rounds_token_amount_up ... ok
test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 8.24s
```
