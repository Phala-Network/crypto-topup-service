# Lock exposure near cap exercise

Date: 2026-09-22.

Status: partial; blocked on C10 atomic reservation/cap enforcement.

G2 exercised once: [ ]

A task-scoped PostgreSQL 16 database was migrated, then seeded with two products, three accounts,
four lock addresses, and four unexpired/unconsumed locks with non-zero `credit_minor` values:

```sh
docker run -d --rm --name wp-d5-review-postgres -e POSTGRES_PASSWORD=postgres \
  -p 127.0.0.1:55435:5432 postgres:16
docker exec wp-d5-review-postgres createdb -U postgres wp_d5_locks
SQLX_OFFLINE=true \
  MIGRATE_DATABASE_URL=postgres://postgres:postgres@127.0.0.1:55435/wp_d5_locks \
  cargo run --locked -q -p topup -- migrate
docker exec -i wp-d5-review-postgres psql -v ON_ERROR_STOP=1 -U postgres \
  -d wp_d5_locks <<'SQL'
DO $$ BEGIN
  IF NOT EXISTS (SELECT 1 FROM pg_roles WHERE rolname='wp_d5_app') THEN
    CREATE ROLE wp_d5_app LOGIN PASSWORD 'wp_d5_app' IN ROLE topup_app;
  END IF;
END $$;
INSERT INTO products (id,slug,settlement_url,webhook_url,pubkey,kid) VALUES
 ('10000000-0000-0000-0000-000000000001','mock-a','http://mock/settle','http://mock/hook','mock','mock/v1'),
 ('10000000-0000-0000-0000-000000000002','mock-b','http://mock/settle','http://mock/hook','mock','mock/v1');
INSERT INTO accounts (id,product_id,external_id) VALUES
 ('20000000-0000-0000-0000-000000000001','10000000-0000-0000-0000-000000000001','acct-a'),
 ('20000000-0000-0000-0000-000000000002','10000000-0000-0000-0000-000000000001','acct-b'),
 ('20000000-0000-0000-0000-000000000003','10000000-0000-0000-0000-000000000002','acct-c');
INSERT INTO addresses (id,account_id,chain_id,kind,version,lock_ref,salt,address) VALUES
 ('30000000-0000-0000-0000-000000000001','20000000-0000-0000-0000-000000000001',31337,'lock',1,'lock-a1','0x0000000000000000000000000000000000000000000000000000000000000001','0x0000000000000000000000000000000000000001'),
 ('30000000-0000-0000-0000-000000000002','20000000-0000-0000-0000-000000000001',31337,'lock',1,'lock-a2','0x0000000000000000000000000000000000000000000000000000000000000002','0x0000000000000000000000000000000000000002'),
 ('30000000-0000-0000-0000-000000000003','20000000-0000-0000-0000-000000000002',31337,'lock',1,'lock-b1','0x0000000000000000000000000000000000000000000000000000000000000003','0x0000000000000000000000000000000000000003'),
 ('30000000-0000-0000-0000-000000000004','20000000-0000-0000-0000-000000000003',31337,'lock',1,'lock-c1','0x0000000000000000000000000000000000000000000000000000000000000004','0x0000000000000000000000000000000000000004');
INSERT INTO rate_locks (address_id,route,amount_atomic,price_scaled,credit_minor,expires_at) VALUES
 ('30000000-0000-0000-0000-000000000001','mock-route',100,100000000,120000,now()+interval '15 minutes'),
 ('30000000-0000-0000-0000-000000000002','mock-route',100,100000000,380000,now()+interval '15 minutes'),
 ('30000000-0000-0000-0000-000000000003','mock-route',100,100000000,1000000,now()+interval '15 minutes'),
 ('30000000-0000-0000-0000-000000000004','mock-route',100,100000000,2000000,now()+interval '15 minutes');
SQL
docker exec -e PGPASSWORD=wp_d5_app -i wp-d5-review-postgres \
  psql -h 127.0.0.1 -U wp_d5_app -d wp_d5_locks -v ON_ERROR_STOP=1 \
  --set=account_cap=500000 --set=product_cap=5000000 --set=global_cap=10000000 <<'SQL'
BEGIN TRANSACTION READ ONLY;
WITH open_locks AS (
  SELECT rl.route,a.id AS account_id,p.id AS product_id,rl.credit_minor
  FROM rate_locks rl
  JOIN addresses ad ON ad.id=rl.address_id
  JOIN accounts a ON a.id=ad.account_id
  JOIN products p ON p.id=a.product_id
  WHERE rl.consumed_by IS NULL AND rl.expires_at>now()
), exposure AS (
  SELECT route,account_id,product_id,
         sum(credit_minor) OVER (PARTITION BY route,account_id) AS account_open_minor,
         sum(credit_minor) OVER (PARTITION BY route,product_id) AS product_open_minor,
         sum(credit_minor) OVER (PARTITION BY route) AS global_open_minor
  FROM open_locks
)
SELECT route,account_id,product_id,
       max(account_open_minor)::text AS account_open_minor,
       :'account_cap' AS account_cap,
       max(product_open_minor)::text AS product_open_minor,
       :'product_cap' AS product_cap,
       max(global_open_minor)::text AS global_open_minor,
       :'global_cap' AS global_cap
FROM exposure
GROUP BY route,account_id,product_id
ORDER BY route,product_id,account_id;
COMMIT;
SQL
```

Observed from the exact read-only query in the runbook:

```text
route      account_id                            product_id                            account_open_minor account_cap product_open_minor product_cap global_open_minor global_cap
mock-route 20000000-0000-0000-0000-000000000001 10000000-0000-0000-0000-000000000001 500000            500000      1500000            5000000     3500000           10000000
mock-route 20000000-0000-0000-0000-000000000002 10000000-0000-0000-0000-000000000001 1000000           500000      1500000            5000000     3500000           10000000
mock-route 20000000-0000-0000-0000-000000000003 10000000-0000-0000-0000-000000000002 2000000           500000      2000000            5000000     3500000           10000000
(3 rows)
```

This verifies aggregation of `rate_locks.credit_minor` by account, product, and route-global scope
with non-zero data. Quote rejection and atomic reservation at the configured caps remain blocked
until C10 lands, so the G2 box remains unchecked.
