# Restore exercise

Date: 2026-09-22.

Status: partial. The C8 post-restore gate ran against seeded data; restore execution is blocked on
D3's `deploy/RESTORE.md` and an implemented `topup restore-check` in #58.

G2 exercised once: [ ]

```sh
topup restore-check
```

```text
restore-check is not implemented
exit=1
```

The migration-state query ran as the application role against a database freshly migrated from
this branch (15 `*.up.sql` files):

```text
BEGIN
    version
----------------
 20260922000018
(1 row)

 applied | failed
---------+--------
      15 |      0
(1 row)

COMMIT
```

The post-restore gate ran with the application role against the reconciliation exercise database
from [local setup](local-setup.md), where no deposit was at or beyond `cleared`:

```sh
topup reconcile --once --post-restore --route /tmp/wp-d5-ex/route.yaml
```

```text
{"message":"reconciler heartbeat","findings":3,"post_restore":true}
{"message":"reconciliation completed","findings":3}
exit=0
```

Then a `cleared` deposit with a `sent` settlement was seeded, standing in for a snapshot taken
before the product answered. With the product unreachable and no settlement key available to the
host process, the gate failed closed:

```sql
INSERT INTO deposits (id,chain_id,tx_hash,log_index,block_number,block_hash,block_time,address_id,account_id,
  route,route_version,asset_contract,from_address,amount_atomic,state,next_attempt_at,
  valuation_at,price_scaled,price_source,credit_minor) VALUES
 ('40000000-0000-0000-0000-000000000002',31337,
  '0x6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b6b',0,1,
  '0x6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c6c',now(),
  '30000000-0000-0000-0000-000000000002','20000000-0000-0000-0000-000000000002',
  'local-anvil-pha-usd',1,'0x5fbdb2315678afecb367f032d93f642f64180aa3',
  '0x00000000000000000000000000000000000000c9',1000000000000000000,'cleared',now(),
  now(),100000000,'spot',100);
INSERT INTO settlements (deposit_id,product_id,key,payload,status,sent_at) VALUES
 ('40000000-0000-0000-0000-000000000002','10000000-0000-0000-0000-000000000001',
  'deposit:40000000-0000-0000-0000-000000000002','{"version":1}','sent',now());
```

```text
{"level":"WARN","fields":{"message":"product settlement lookup failed","deposit_id":"40000000-0000-0000-0000-000000000002","error":"signing key is unavailable"}}
{"level":"WARN","fields":{"message":"reconciliation mismatch","check":"post_restore_settlement","subjects":"{\"deposit_id\": \"40000000-0000-0000-0000-000000000002\", \"key\": \"deposit:40000000-0000-0000-0000-000000000002\"}","expected":"{\"product_answer\":\"available\"}","observed":"{\"error\":\"settlement_lookup_failed\",\"local_state\":\"cleared\"}","metric":"topup_reconciliation_mismatches_total"}}
{"level":"ERROR","fields":{"message":"post-restore reconciliation is incomplete","findings":5}}
exit=1
```

Adoption of an available product answer is covered by
`post_restore_completes_when_product_truth_is_adopted` in the
[reconciliation exercise](reconciliation-mismatch.md). The runbook's read-only restore queries ran
as `wp_d5_app` against the migrated schema (see [local setup](local-setup.md#runbook-sql)). Backup
listing, encrypted restore, and RPO/RTO evidence remain blocked on #58.
