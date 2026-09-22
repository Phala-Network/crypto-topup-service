# Lock expiry worker failure

## Trigger

Trigger on `rate-lock expiry scan failed` errors in the service log, open rate locks more than a
minute past `expires_at`, missing `rate_lock.expired` events, or `overdue_locks > 0` persisting in
the [Lock exposure near cap](lock-exposure-near-cap.md) query. PR #56 registers no heartbeat for
this loop, so `TopupLoopStopped` does not cover it; that metric gap must be closed when #56 is
rebased onto C10.

## Impact and blast radius

The C10 worker scans every five seconds and, in one transaction per batch of 100, marks overdue open
locks `expired`, releases their `lock_exposure` reservations, and queues `rate_lock.expired`. When it
fails, expired locks keep holding exposure, so new quotes reach `409 exposure_cap_exceeded` early,
and products are not told that checkouts expired. One lock whose release fails rolls back its whole
batch, so every other overdue lock in that batch stays held as well. Payment handling does not
depend on the worker: the confirm step still judges a payment by `expires_at` and the route
tolerance. Blast radius is every quote-first route sharing the database.

## First 5 minutes

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --since 30m topup | grep -F 'rate-lock expiry scan failed'
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
- Errors say `rate-lock database invariant failed` every tick and the ledger differs from the
  recomputation: a release would drive a counter below zero, so the batch rolls back each time and a
  restart will not help. Treat it as a reconciliation incident; pause `quotes` if exposure is near
  cap, and escalate to Engineering for the owner repair below.

## Remediation

Restart only after preserving logs; the worker resumes from database state:

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=500 topup > /tmp/topup-before-restart.log
docker compose -f deploy/docker-compose.staging.yml restart topup
```

Never set `rate_locks.status` or `exposure_reserved` by hand. A ledger inconsistency needs a
reviewed forward repair by the database owner, outside the service container (**HUMAN-ONLY**): set
each drifted counter to the `open_reserved_minor` value from the exposure query, record the before
and after values in the incident, and let the worker release the overdue locks on its next tick.
Never place owner credentials in the service container.

## Verification

Within a minute of recovery, the overdue-lock count is zero, `rate_lock.expired` events appear for
the drained locks, the ledger equals the recomputation, and no new `rate-lock expiry scan failed`
lines appear. Resume `quotes` if it was paused.

## Rollback

If a restart does not help, re-pause `quotes` and redeploy the retained prior compose hash. Expired
locks are never re-opened.
