# Chain frozen exercise

Date: 2026-09-22.

Status: complete local CLI scenario against PostgreSQL 16 and Anvil, continuing from the
[reconciliation mismatch exercise](reconciliation-mismatch.md) in database `wp_d5_recon2`.

G2 exercised once: [x]

The first reconciliation pass wrote `chain:31337` for the address whose stored value differs from
the factory. The runbook's query as `wp_d5_app` with `--set=chain_id=31337` returned:

```text
BEGIN
  block_key  |     check_name     |                        reason                         |          created_at
-------------+--------------------+-------------------------------------------------------+-------------------------------
 chain:31337 | address_derivation | factory addressOf(salt) disagrees with stored address | 2026-09-22 20:30:02.257426+00
(1 row)

              address_id              |                                salt                                |              factory_address               |               stored_address               |    kind    | version | lock_ref |              account_id              |          created_at
--------------------------------------+--------------------------------------------------------------------+--------------------------------------------+--------------------------------------------+------------+---------+----------+--------------------------------------+-------------------------------
 30000000-0000-0000-0000-000000000002 | 0x0202020202020202020202020202020202020202020202020202020202020202 | 0x87f8d7a18301534bea9d23eae0a677626b2dd819 | 0x0000000000000000000000000000000000000bad | persistent |       1 |          | 20000000-0000-0000-0000-000000000002 | 2026-09-22 20:30:02.267109+00
(1 row)

COMMIT
```

Both providers (Anvil through `127.0.0.1` and `localhost`) and the contract tuple:

```sh
cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'addressOf(bytes32)(address)' "$SALT" --rpc-url "$RPC_PROVIDER_B_URL"
cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
```

```text
0x87f8D7a18301534bEa9D23eAE0a677626b2Dd819
0x87f8D7a18301534bEa9D23eAE0a677626b2Dd819
0xCafac3dD18aC6c6e92c921884f9E4176737C052c
0x70997970C51812dc3A010C7d01b50e0d17dc79C8
```

Providers agree and the contracts match the route, so the stored row is wrong. Lifting the block as
the owner without fixing the cause does not unfreeze the chain: the next pass writes it again.

```text
owner DELETE chain:31337 -> DELETE 1
topup reconcile -> exit=0
chain blocks for 31337 -> 1
```

After the owner corrected the stored address (standing in for the reviewed repair) and lifted the
block, one pass completed and the runbook's verification query returned no chain block:

```text
owner UPDATE address, DELETE chain:31337 -> UPDATE 1 DELETE 1
{"message":"reconciler heartbeat","findings":2,"post_restore":false}
{"message":"reconciliation completed","findings":2}
topup reconcile -> exit=0
chain blocks for 31337 -> 0
```

The two remaining findings are the unrelated credit and custody mismatches from the previous
exercise. The frozen-chain runtime effects were exercised by integration tests against the same
PostgreSQL instance:

```sh
cargo test --locked -p topup --test api frozen_chain_refuses_address_issuance_and_rate_locks -- --nocapture
cargo test --locked -p topup --test reconciler frozen_chain_gates_startup_pumps_and_scanner -- --nocapture
```

```text
test frozen_chain_refuses_address_issuance_and_rate_locks ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 4 filtered out; finished in 2.93s
test frozen_chain_gates_startup_pumps_and_scanner ... ok
test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 10 filtered out; finished in 0.97s
```

They assert `423 chain_frozen` from address issuance and rate-lock creation, and that pumps and the
scanner hold the frozen chain while another chain proceeds.
