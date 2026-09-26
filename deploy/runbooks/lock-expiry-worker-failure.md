# Lock expiry worker failure

## Trigger

Trigger on `TopupLockExpiryFailing` (an expiry scan failed), missed check-ins of the Sentry Crons
monitor `topup-lock-expiry` (no successful scan for five minutes),
`rate-lock expiry scan failed` errors in the service log, `overdue_locks > 0` for more than a
minute in the query below or in the [Lock exposure near cap](lock-exposure-near-cap.md) query, or
missing `rate_lock.expired` events.
A lock is overdue only once the worker could expire it: its chain's scanner has committed through a
finalized block whose time is past `expires_at`, and no payment mined inside the window still awaits
its confirm step (architecture §9). A lock merely past `expires_at` by wall clock is still waiting
for finality, about 15 minutes, and is not a worker failure.
`topup-lock-expiry` checks in only after a successful scan, so a scan that keeps failing misses
its check-ins as well as raising `TopupLockExpiryFailing`.

## Impact and blast radius

The C10 worker scans every five seconds and, in one transaction per batch of 100, marks overdue open
locks `expired`, which releases their exposure, and queues `rate_lock.expired`. When it fails,
overdue locks keep holding exposure, so new quotes reach `409 exposure_cap_exceeded` early, and
products are not told that checkouts expired. A scan that finds nothing to expire,
including while the scanner is stalled, still succeeds, so `TopupLockExpiryFailing` means scans are
failing; a stalled scanner holds locks open by design and pages as `topup-scanner-<chain_id>`. A
failure rolls back the whole batch, and because batches are taken oldest `expires_at` first, the
same locks are selected again on every tick: the worker makes no progress at all until the cause is
repaired.
Payment handling does not depend on the worker: the confirm step still judges a payment by
`expires_at` and the route tolerance. Blast radius is every quote-first route sharing the database.

## First 5 minutes

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --since 30m topup | grep 'rate-lock expiry scan failed'
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT count(*) FILTER (WHERE e.expirable) AS overdue_locks,
       min(rl.expires_at) FILTER (WHERE e.expirable) AS oldest_expiry,
       coalesce(sum(rl.credit_minor) FILTER (WHERE e.expirable AND rl.exposure_reserved),0)::text
         AS held_minor,
       count(*) FILTER (WHERE NOT e.expirable AND rl.expires_at <= now() - interval '30 minutes')
         AS awaiting_chain_30m
FROM rate_locks rl
JOIN addresses ad ON ad.id=rl.address_id
LEFT JOIN cursors c ON c.chain_id=ad.chain_id
CROSS JOIN LATERAL (
  SELECT coalesce(rl.expires_at < c.scanned_block_time, false) AND NOT EXISTS (
    SELECT 1 FROM deposits d
    WHERE d.address_id=rl.address_id AND d.state='detected' AND d.block_time <= rl.expires_at
  ) AS expirable
) e
WHERE rl.status='open' AND rl.consumed_by IS NULL AND rl.expires_at <= now();
SELECT max(created_at) AS last_expired_event,
       count(*) FILTER (WHERE created_at > now() - interval '1 hour') AS expired_events_1h
FROM outbox WHERE event_type='rate_lock.expired';
COMMIT;
SQL
```

Then run the query in [Lock exposure near cap](lock-exposure-near-cap.md) to see how close each
scope is to its cap.

## Decision tree

- No error logs and no overdue locks: the worker is healthy; close the alert.
- `awaiting_chain_30m > 0` with no overdue locks: the worker is waiting for chain time, as designed.
  Either the scanner's finalized cursor is stalled (`topup-scanner-<chain_id>`; follow
  [Scanner lag](scanner-lag.md)) or an in-window payment is stuck in `detected`
  (`TopupDepositStateAgeExceeded`, `state:detected`; follow
  [Provider disagreement](provider-disagreement.md)). The locks expire on their own once that
  clears; do not expire them by hand.
- Errors name a database connection or timeout: restore database health; the worker retries on the
  next tick without a restart.
- Overdue locks with no error logs: the worker task has stopped; preserve logs and restart the
  service container.
- Errors say `rate-lock database invariant failed` every tick: stored lock data breaks an
  invariant, so the batch rolls back each time and a restart will not help. Preserve logs and
  escalate to Engineering.

## Remediation

Restart only after preserving logs; the worker resumes from database state:

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=500 topup > /tmp/topup-before-restart.log
docker compose -f deploy/docker-compose.staging.yml restart topup
```

Never set `rate_locks.status` or `exposure_reserved` by hand: the caps are enforced against the
open reserved locks, and a lock closed outside the worker never emits `rate_lock.expired`. Pausing
`quotes` does not stop consume, cancel, or expiry.

## Verification

Within a minute of recovery, the overdue-lock count is zero, `rate_lock.expired` events appear for
the drained locks, no new `rate-lock expiry scan failed` lines appear, and `TopupLockExpiryFailing`
resolves. Resume `quotes` if it was paused.

## Rollback

If a restart does not help, re-pause `quotes` and redeploy the retained prior compose hash. Expired
locks are never re-opened.
