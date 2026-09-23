# Lock expiry worker failure exercise

Date: 2026-09-22.

Status: complete local scenario with a running `topup run` service, the dstack simulator, Anvil,
and PostgreSQL 16 (see [local setup](local-setup.md)). It uses the seeded locks database from the
[lock exposure exercise](lock-exposure-near-cap.md), re-created after merging #70 and #72, with the
owner drifting `product:10000000-0000-0000-0000-000000000002` to 390000 while its reservations
total 400000. The owner repair ran while the service was running.

G2 exercised once: [x]

Started the service on the host against the locks database with the application role:

```sh
mkdir -p /tmp/wp-d5-ex/dstack && chmod 777 /tmp/wp-d5-ex/dstack
docker run -d --rm --name wp-d5-exercise-dstack -v /tmp/wp-d5-ex/dstack:/var/run \
  crypto-topup-dstack-simulator:local
export DATABASE_URL=postgres://wp_d5_app:wp_d5_app@127.0.0.1:55436/wp_d5_locks
export DSTACK_SIMULATOR_ENDPOINT=/tmp/wp-d5-ex/dstack/dstack.sock
export TOPUP_ADMIN_KID=local-admin/v1
export TOPUP_ADMIN_PUBLIC_KEY=11qYAYKxCrfVS/7TyWQHOg7hcvPapiMlrwIaaPcHURo=
export TOPUP_PUBLIC_ORIGIN=http://127.0.0.1:18089
target/debug/topup run --bind 127.0.0.1:18089 --route /tmp/wp-d5-ex/route.yaml \
  > /tmp/wp-d5-ex/service.log 2>&1 &
```

## Healthy worker

Within one tick the worker expired the seeded overdue lock; the runbook's detection query returned:

```text
BEGIN
 overdue_locks | oldest_expiry | held_minor
---------------+---------------+------------
             0 |               | 0
(1 row)

      last_expired_event       | expired_events_1h
-------------------------------+-------------------
 2026-09-22 21:01:36.728975+00 |                 1
(1 row)

COMMIT
```

## Fault

The owner expired the lock whose product counter had drifted, so its release would drive the
counter below zero:

```sh
docker exec -i wp-d5-exercise-pg psql -U postgres -d wp_d5_locks \
  -c "UPDATE rate_locks SET expires_at=now()-interval '2 minutes' WHERE address_id='30000000-0000-0000-0000-000000000005';"
grep -F 'rate-lock expiry scan failed' /tmp/wp-d5-ex/service.log
```

```text
{"level":"ERROR","fields":{"message":"rate-lock expiry scan failed","error":"rate-lock database invariant failed"}}
{"level":"ERROR","fields":{"message":"rate-lock expiry scan failed","error":"rate-lock database invariant failed"}}
{"level":"ERROR","fields":{"message":"rate-lock expiry scan failed","error":"rate-lock database invariant failed"}}
```

The runbook's detection query and the exposure query from
[Lock exposure near cap](../lock-exposure-near-cap.md):

```text
BEGIN
 overdue_locks |         oldest_expiry         | held_minor
---------------+-------------------------------+------------
             1 | 2026-09-22 20:59:52.149958+00 | 400000
(1 row)

      last_expired_event       | expired_events_1h
-------------------------------+-------------------
 2026-09-22 21:01:36.728975+00 |                 1
(1 row)

COMMIT
BEGIN
                  scope_key                   | ledger_open_minor | open_reserved_minor | unexpired_minor | overdue_locks | cap_minor | ledger_bps_of_cap
----------------------------------------------+-------------------+---------------------+-----------------+---------------+-----------+-------------------
 account:20000000-0000-0000-0000-000000000001 | 450000            | 450000              | 450000          |             0 | 500000    |              9000
 account:20000000-0000-0000-0000-000000000003 | 400000            | 400000              | 0               |             1 | 500000    |              8000
 account:20000000-0000-0000-0000-000000000002 | 100000            | 100000              | 100000          |             0 | 500000    |              2000
 product:10000000-0000-0000-0000-000000000001 | 550000            | 550000              | 550000          |             0 | 5000000   |              1100
 global                                       | 950000            | 950000              | 550000          |             1 | 10000000  |               950
 product:10000000-0000-0000-0000-000000000002 | 390000            | 400000              | 0               |             1 | 5000000   |               780
(6 rows)

COMMIT
```

The overdue lock stays `open`, its 400000 stays reserved in all three scopes, and the drifted
product counter shows as `ledger_open_minor` 390000 against `open_reserved_minor` 400000.

## Owner repair with the service running

The runbook's owner SQL, unchanged, as the database owner with
`--set=scope_key='product:10000000-0000-0000-0000-000000000002'`:

```text
BEGIN
                  scope_key                   | before_minor
----------------------------------------------+--------------
 product:10000000-0000-0000-0000-000000000002 | 390000
(1 row)

recomputed_minor=400000
                  scope_key                   | after_minor
----------------------------------------------+-------------
 product:10000000-0000-0000-0000-000000000002 | 400000
(1 row)

UPDATE 1
COMMIT
psql exit=0
failed-scan lines: 4 at the repair, 4 twelve seconds later
```

Verification with the runbook's queries:

```text
BEGIN
 overdue_locks | oldest_expiry | held_minor
---------------+---------------+------------
             0 |               | 0
(1 row)

      last_expired_event       | expired_events_1h
-------------------------------+-------------------
 2026-09-22 21:02:16.649858+00 |                 2
(1 row)

COMMIT
BEGIN
                  scope_key                   | ledger_open_minor | open_reserved_minor | unexpired_minor | overdue_locks | cap_minor | ledger_bps_of_cap
----------------------------------------------+-------------------+---------------------+-----------------+---------------+-----------+-------------------
 account:20000000-0000-0000-0000-000000000001 | 450000            | 450000              | 450000          |             0 | 500000    |              9000
 account:20000000-0000-0000-0000-000000000002 | 100000            | 100000              | 100000          |             0 | 500000    |              2000
 product:10000000-0000-0000-0000-000000000001 | 550000            | 550000              | 550000          |             0 | 5000000   |              1100
 global                                       | 550000            | 550000              | 550000          |             0 | 10000000  |               550
(4 rows)

COMMIT
```

The held lock expired on the next tick, every counter equals the recomputation, and no further scan
failures appeared. The service then stopped cleanly on `SIGTERM`:

```text
{"message":"topup service stopped","stuck_deposit_alerts":0,"reconciliation_heartbeat":1790110896,"rate_lock_expiry_heartbeats":12,"rate_locks_expired":2}
```

```sh
docker stop wp-d5-exercise-dstack
```

The stop-repair-start alternative uses the same SQL without concurrent writers and was not run
separately. The runbook now repairs drift in service with `topup reconcile` (the
`lock_exposure` check, #75); its concurrency behaviour is covered by
`exposure_repair_converges_under_concurrent_create_consume_cancel_and_expire` in
`crates/topup/tests/rate_locks.rs`, and this owner-SQL exercise has not been re-run against it.
