# Reconciliation mismatch exercise

Date: 2026-09-22.

Status: complete local CLI scenario against PostgreSQL 16 and Anvil (see
[local setup](local-setup.md), database `wp_d5_recon2`).

G2 exercised once: [x]

Seeded one product and two accounts: a correctly derived address with a `confirmed` deposit whose
stored `credit_minor` is `101` instead of the recomputed `100` and whose forwarder holds no tokens,
and a second address whose stored value is not the factory's `addressOf(salt)`:

```sh
docker exec -i wp-d5-exercise-pg psql -U postgres -d wp_d5_recon2 -v ON_ERROR_STOP=1 -q \
  < /tmp/wp-d5-ex/seed-recon.sql
```

```sql
INSERT INTO products (id,slug,settlement_url,webhook_url,pubkey,kid) VALUES
 ('10000000-0000-0000-0000-000000000001','mock-product','http://127.0.0.1:9/settlements','http://127.0.0.1:9/hooks','mock','mock/v1');
INSERT INTO accounts (id,product_id,external_id) VALUES
 ('20000000-0000-0000-0000-000000000001','10000000-0000-0000-0000-000000000001','workspace-1'),
 ('20000000-0000-0000-0000-000000000002','10000000-0000-0000-0000-000000000001','workspace-2');
INSERT INTO addresses (id,account_id,chain_id,kind,version,salt,address) VALUES
 ('30000000-0000-0000-0000-000000000001','20000000-0000-0000-0000-000000000001',31337,'persistent',1,
  '0x0101010101010101010101010101010101010101010101010101010101010101','0x7872de4d4b95c553c952acc143486f821d45e923'),
 ('30000000-0000-0000-0000-000000000002','20000000-0000-0000-0000-000000000002',31337,'persistent',1,
  '0x0202020202020202020202020202020202020202020202020202020202020202','0x0000000000000000000000000000000000000bad');
INSERT INTO deposits (id,chain_id,tx_hash,log_index,block_number,block_hash,block_time,address_id,account_id,
  route,route_version,asset_contract,from_address,amount_atomic,state,next_attempt_at,
  valuation_at,price_scaled,price_source,credit_minor) VALUES
 ('40000000-0000-0000-0000-000000000001',31337,
  '0x5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b5b',0,1,
  '0x5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c5c',now(),
  '30000000-0000-0000-0000-000000000001','20000000-0000-0000-0000-000000000001',
  'local-anvil-pha-usd',1,'0x5fbdb2315678afecb367f032d93f642f64180aa3',
  '0x00000000000000000000000000000000000000c9',1000000000000000000,'confirmed',now(),
  now(),100000000,'spot',101);
```

One reconciliation pass with the application role:

```sh
topup reconcile --once --route /tmp/wp-d5-ex/route.yaml
```

Observed log fields (`jq -c .fields`), exit `0`:

```text
{"message":"reconciliation mismatch","check":"address_derivation","subjects":"{\"address_id\": \"30000000-0000-0000-0000-000000000002\", \"chain_id\": \"31337\", \"salt\": \"0x0202020202020202020202020202020202020202020202020202020202020202\"}","expected":"{\"address\":\"0x87f8d7a18301534bea9d23eae0a677626b2dd819\"}","observed":"{\"address\":\"0x0000000000000000000000000000000000000bad\"}","metric":"topup_reconciliation_mismatches_total"}
{"message":"reconciliation mismatch","check":"credit_recomputation","subjects":"{\"address_id\": \"30000000-0000-0000-0000-000000000001\", \"chain_id\": \"31337\", \"deposit_id\": \"40000000-0000-0000-0000-000000000001\"}","expected":"{\"credit_minor\":\"100\"}","observed":"{\"credit_minor\":\"101\"}","metric":"topup_reconciliation_mismatches_total"}
{"message":"reconciliation mismatch","check":"custody_balance","subjects":"{\"address_id\": \"30000000-0000-0000-0000-000000000001\", \"chain_id\": \"31337\", \"token\": \"0x5fbdb2315678afecb367f032d93f642f64180aa3\"}","expected":"{\"deposits_atomic\":\"1000000000000000000\",\"flushed_atomic\":\"0\"}","observed":"{\"balance_atomic\":\"0\"}","metric":"topup_reconciliation_mismatches_total"}
{"message":"reconciler heartbeat","findings":3,"post_restore":false}
{"message":"reconciliation completed","findings":3}
```

The runbook's first-5-minutes query as `wp_d5_app`:

```text
BEGIN
      check_name      |                                                                         subjects                                                                          |                             expected                              |                         observed                          | repair_applied | incomplete |          created_at
----------------------+-----------------------------------------------------------------------------------------------------------------------------------------------------------+-------------------------------------------------------------------+-----------------------------------------------------------+----------------+------------+-------------------------------
 custody_balance      | {"token": "0x5fbdb2315678afecb367f032d93f642f64180aa3", "chain_id": "31337", "address_id": "30000000-0000-0000-0000-000000000001"}                        | {"flushed_atomic": "0", "deposits_atomic": "1000000000000000000"} | {"balance_atomic": "0"}                                   | f              | f          | 2026-09-22 20:30:02.455437+00
 credit_recomputation | {"chain_id": "31337", "address_id": "30000000-0000-0000-0000-000000000001", "deposit_id": "40000000-0000-0000-0000-000000000001"}                         | {"credit_minor": "100"}                                           | {"credit_minor": "101"}                                   | f              | f          | 2026-09-22 20:30:02.414707+00
 address_derivation   | {"salt": "0x0202020202020202020202020202020202020202020202020202020202020202", "chain_id": "31337", "address_id": "30000000-0000-0000-0000-000000000002"} | {"address": "0x87f8d7a18301534bea9d23eae0a677626b2dd819"}         | {"address": "0x0000000000000000000000000000000000000bad"} | f              | f          | 2026-09-22 20:30:02.267109+00
(3 rows)

                  block_key                   |  scope  | chain_id |              address_id              |      check_name      |                          reason                          |          created_at
----------------------------------------------+---------+----------+--------------------------------------+----------------------+----------------------------------------------------------+-------------------------------
 chain:31337                                  | chain   |    31337 |                                      | address_derivation   | factory addressOf(salt) disagrees with stored address    | 2026-09-22 20:30:02.257426+00
 address:30000000-0000-0000-0000-000000000001 | address |    31337 | 30000000-0000-0000-0000-000000000001 | credit_recomputation | stored credit disagrees with deterministic recomputation | 2026-09-22 20:30:02.412068+00
(2 rows)

         action          |                                                                       subject                                                                        |                                                                                 reason                                                                                 |          created_at
-------------------------+------------------------------------------------------------------------------------------------------------------------------------------------------+------------------------------------------------------------------------------------------------------------------------------------------------------------------------+-------------------------------
 reconciliation_mismatch | {"address_id":"30000000-0000-0000-0000-000000000001","chain_id":"31337","token":"0x5fbdb2315678afecb367f032d93f642f64180aa3"}                        | {"check":"custody_balance","expected":{"deposits_atomic":"1000000000000000000","flushed_atomic":"0"},"observed":{"balance_atomic":"0"}}                                | 2026-09-22 20:30:02.455437+00
 reconciliation_mismatch | {"address_id":"30000000-0000-0000-0000-000000000001","chain_id":"31337","deposit_id":"40000000-0000-0000-0000-000000000001"}                         | {"check":"credit_recomputation","expected":{"credit_minor":"100"},"observed":{"credit_minor":"101"}}                                                                   | 2026-09-22 20:30:02.414707+00
 reconciliation_mismatch | {"address_id":"30000000-0000-0000-0000-000000000002","chain_id":"31337","salt":"0x0202020202020202020202020202020202020202020202020202020202020202"} | {"check":"address_derivation","expected":{"address":"0x87f8d7a18301534bea9d23eae0a677626b2dd819"},"observed":{"address":"0x0000000000000000000000000000000000000bad"}} | 2026-09-22 20:30:02.267109+00
(3 rows)

COMMIT
```

A second pass logged no new mismatch and `SELECT count(*) FROM reconciliation_findings` stayed
`3`, so findings and audit rows are written once. The application role cannot lift a block:

```text
ERROR:  permission denied for table reconciliation_blocks
```

The chain-level block and its lifting are exercised in [Chain frozen](chain-frozen.md). The C8
integration suite also passed against the same PostgreSQL instance, including repairs, sent
settlement adoption, and idempotent scoped blocks:

```sh
cargo test --locked -p topup --test reconciler -- --nocapture
```

```text
test application_role_cannot_rewrite_findings_or_delete_blocks ... ok
test sent_settlements_are_adopted_under_a_lease_and_waits_are_quiet ... ok
test frozen_chain_gates_startup_pumps_and_scanner ... ok
test mismatches_block_only_required_scopes_and_findings_are_idempotent ... ok
test loop_respects_cancellation ... ok
test repairs_missing_deposits_incrementally_and_links_flushes_with_audit ... ok
test checks_are_independent_and_heartbeat_requires_a_successful_round ... ok
test post_restore_stays_incomplete_without_verified_product_truth ... ok
test post_restore_completes_when_product_truth_is_adopted ... ok
test custody_balances_use_the_finalized_block_and_incremental_totals ... ok
test flush_linkage_keeps_a_state_advanced_after_the_scan ... ok
test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 12.01s
```
