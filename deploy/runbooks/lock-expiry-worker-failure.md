# Lock expiry worker failure

## Trigger

Trigger on `TopupLockExpiryFailing` (`topup_lock_expiry_failures_total` increasing for five
minutes), `TopupLockExposureDrift` (the reconciler repaired a `lock_exposure` counter),
`TopupLoopStopped{loop="lock_expiry"}`, `rate-lock expiry scan failed` errors in the service log,
open rate locks more than a minute past `expires_at`, missing `rate_lock.expired` events, or
`overdue_locks > 0` persisting in the [Lock exposure near cap](lock-exposure-near-cap.md) query.
The loop heartbeat only proves the worker is scanning; a scan that keeps failing still heartbeats,
which is what `TopupLockExpiryFailing` covers.

## Impact and blast radius

The C10 worker scans every five seconds and, in one transaction per batch of 100, marks overdue open
locks `expired`, releases their `lock_exposure` reservations, and queues `rate_lock.expired`. When
it fails, expired locks keep holding exposure, so new quotes reach `409 exposure_cap_exceeded`
early, and products are not told that checkouts expired. One lock whose release fails rolls back its
whole batch, and because batches are taken oldest `expires_at` first, the same failing lock is
selected again on every tick: the worker makes no progress at all until the cause is repaired.
Payment handling does not depend on the worker: the confirm step still judges a payment by
`expires_at` and the route tolerance. Blast radius is every quote-first route sharing the database.

## First 5 minutes

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --since 30m topup | grep -E 'rate-lock expiry scan failed|lock exposure counter'
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT count(*) AS overdue_locks,min(expires_at) AS oldest_expiry,
       coalesce(sum(credit_minor) FILTER (WHERE exposure_reserved),0)::text AS held_minor
FROM rate_locks
WHERE status='open' AND consumed_by IS NULL AND expires_at <= now() - interval '1 minute';
SELECT max(created_at) AS last_expired_event,
       count(*) FILTER (WHERE created_at > now() - interval '1 hour') AS expired_events_1h
FROM outbox WHERE event_type='rate_lock.expired';
COMMIT;
SQL
```

Then run the ledger query in [Lock exposure near cap](lock-exposure-near-cap.md) to see whether
`ledger_open_minor` still equals `open_reserved_minor`.

## Decision tree

- No error logs and no overdue locks: the worker is healthy; close the alert.
- Errors name a database connection or timeout: restore database health; the worker retries on the
  next tick without a restart.
- Overdue locks with no error logs: the worker task has stopped; preserve logs and restart the
  service container.
- Errors say `rate-lock database invariant failed` every tick, preceded by `lock exposure counter
  is below the release` naming the `scope_key` and `release_minor`: a release would drive that
  counter below zero, so the batch rolls back each time and a restart will not help. The
  reconciler's `lock_exposure` check repairs the counter on its next round
  (`--reconciliation-interval-s`, default 600 s) and `TopupLockExposureDrift` fires; run the
  in-service repair below to fix it now. Treat any drift as a reconciliation incident: it means a
  writer bypassed the counter, so preserve logs and escalate to Engineering.
- `TopupLockExposureDrift` alone, with no expiry failures: the counter was too high (a capacity
  leak toward the cap) or too low and already repaired. Read the finding as below and escalate.

## Remediation

Restart only after preserving logs; the worker resumes from database state:

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=500 topup > /tmp/topup-before-restart.log
docker compose -f deploy/docker-compose.staging.yml restart topup
```

Never set `rate_locks.status`, `exposure_reserved`, or `lock_exposure.open_minor` by hand:
create, cancel, consume, and expiry keep changing the counters (`global` above all), so a value
copied from an earlier query either under-counts (later releases fail the invariant again and
consuming deposits fail in the confirm step) or over-counts (a permanent leak toward the cap).
Pausing `quotes` does not stop consume, cancel, or expiry.

In-service repair: run one reconciliation pass inside the service container with the application
role. Its `lock_exposure` check finds every counter that differs from the sum of `credit_minor` over
its scope's open reserved locks, then repairs each one in its own transaction: lock the counter
row, recompute, update. Every writer changes `rate_locks` and that row in one transaction and must
take the row's lock, so the recompute sees every committed change and a writer still in flight
applies its own change after the repair. It is safe while the service runs and is idempotent:

```sh
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup reconcile --once --route /etc/topup/routes/phala-cloud-sepolia-pha.yaml
```

Each repaired counter is a `lock_exposure` finding with `repair_applied = true`, `observed` (before)
and `expected` (after), plus a `reconciliation_repair` audit row. Record them in the incident:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT created_at, subjects->>'scope_key' AS scope_key,
       observed->>'open_minor' AS before_minor, expected->>'open_minor' AS after_minor
FROM reconciliation_findings
WHERE check_name = 'lock_exposure'
ORDER BY created_at DESC LIMIT 20;
COMMIT;
SQL
```

## Verification

Within a minute of recovery, the overdue-lock count is zero, `rate_lock.expired` events appear for
the drained locks, the ledger equals the recomputation, a second `topup reconcile --once` records no
new `lock_exposure` finding, no new `rate-lock expiry scan failed` lines appear, and
`TopupLockExpiryFailing` resolves. Resume `quotes` if it was paused.

## Rollback

If a restart does not help, re-pause `quotes` and redeploy the retained prior compose hash. Expired
locks are never re-opened.
