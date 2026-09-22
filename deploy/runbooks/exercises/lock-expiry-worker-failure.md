# Lock expiry worker failure exercise

Date: 2026-09-22.

Status: complete local scenario with a running `topup run` service, the dstack simulator, Anvil,
and PostgreSQL 16 (see [local setup](local-setup.md)). It continues from the
[lock exposure exercise](lock-exposure-near-cap.md) database `wp_d5_locks`, where the owner had
drifted `product:10000000-0000-0000-0000-000000000002` to 390000 while its reservations total
400000.

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
target/debug/topup run --bind 127.0.0.1:18089 --route /tmp/wp-d5-ex/route.yaml \
  > /tmp/wp-d5-ex/service.log 2>&1 &
```

## Healthy worker

Within one tick the worker expired the seeded overdue lock. The runbook's first-5-minutes query:

```text
overdue_locks=0 held_minor=0 expired_events_1h=1
```

`account:...0002` dropped from 150000 to 100000, `product:...0001` from 600000 to 550000, and
`global` from 1000000 to 950000, each equal to the recomputation.

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
             1 | 2026-09-22 20:30:23.776433+00 | 400000
(1 row)

      last_expired_event       | expired_events_1h
-------------------------------+-------------------
 2026-09-22 20:32:00.814002+00 |                 1
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
product counter is visible as `ledger_open_minor` 390000 against `open_reserved_minor` 400000.

## Owner repair and verification

Following the runbook, the owner set the drifted counter to its `open_reserved_minor` value:

```sh
docker exec -i wp-d5-exercise-pg psql -U postgres -d wp_d5_locks \
  -c "UPDATE lock_exposure SET open_minor=400000 WHERE scope_key='product:10000000-0000-0000-0000-000000000002';"
```

```text
UPDATE 1
failed-scan lines: 5 at the repair, 5 twelve seconds later
BEGIN
 overdue_locks | oldest_expiry | held_minor
---------------+---------------+------------
             0 |               | 0
(1 row)

      last_expired_event       | expired_events_1h
-------------------------------+-------------------
 2026-09-22 20:32:50.657207+00 |                 3
(1 row)

COMMIT
BEGIN
                  scope_key                   | ledger_open_minor | open_reserved_minor | unexpired_minor | overdue_locks | cap_minor | ledger_bps_of_cap
----------------------------------------------+-------------------+---------------------+-----------------+---------------+-----------+-------------------
 account:20000000-0000-0000-0000-000000000001 | 120000            | 120000              | 120000          |             0 | 500000    |              2400
 account:20000000-0000-0000-0000-000000000002 | 100000            | 100000              | 100000          |             0 | 500000    |              2000
 product:10000000-0000-0000-0000-000000000001 | 220000            | 220000              | 220000          |             0 | 5000000   |               440
 global                                       | 220000            | 220000              | 220000          |             0 | 10000000  |               220
(4 rows)

COMMIT
```

The held lock expired on the next tick. The third `rate_lock.expired` event is `lock-a2`, which
reached its own `expires_at` at the same time. Every counter equals the recomputation and no further
scan failures appeared. The service then stopped cleanly on
`SIGTERM`:

```text
{"message":"topup service stopped","stuck_deposit_alerts":0,"reconciliation_heartbeat":1790109123,"rate_lock_expiry_heartbeats":14,"rate_locks_expired":3}
```

```sh
docker stop wp-d5-exercise-dstack
```

A heartbeat metric and alert for this loop remain a gap (not in PR #56).
