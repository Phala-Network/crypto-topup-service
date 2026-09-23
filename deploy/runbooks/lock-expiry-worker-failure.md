# Lock expiry worker failure

## Trigger

Trigger on `rate-lock expiry scan failed` errors in the service log, open rate locks more than a
minute past `expires_at`, missing `rate_lock.expired` events, or `overdue_locks > 0` persisting in
the [Lock exposure near cap](lock-exposure-near-cap.md) query, or `TopupLoopStopped{loop="lock_expiry"}`.
The loop heartbeat only proves the worker is scanning; a scan that keeps failing still heartbeats,
so also watch `time() - topup_loop_progress_unixtime_seconds{loop="lock_expiry"}`.

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

Never set `rate_locks.status` or `exposure_reserved` by hand, and never write a counter value copied
from an earlier query: create, cancel, consume, and expiry keep changing the counters (`global`
above all), so a stale value either under-counts (later releases fail the invariant again and
consuming deposits fail in the confirm step) or over-counts (a permanent leak toward the cap).
Pausing `quotes` does not stop consume, cancel, or expiry.

**HUMAN-ONLY, database owner, outside the service container:** repair one drifted scope key per
transaction with the exact SQL below. It locks the counter row first; every writer changes
`rate_locks` and this row in the same transaction and must take this row's lock, so under the
default `READ COMMITTED` isolation the recompute (a separate statement with a fresh snapshot) sees
every committed change and nothing can commit in between. Record the printed before, recomputed,
and after values in the incident. `$OWNER_DATABASE_URL` is the owner connection from the secret
manager on an operator host; never place owner credentials in the service container.

```sh
export SCOPE_KEY='product:<product_id>'
psql "$OWNER_DATABASE_URL" -v ON_ERROR_STOP=1 --set=scope_key="$SCOPE_KEY" <<'SQL'
BEGIN;
SELECT scope_key,open_minor::text AS before_minor FROM lock_exposure
WHERE scope_key = :'scope_key' FOR UPDATE;
SELECT coalesce(sum(rl.credit_minor),0)::text AS recomputed_minor
FROM rate_locks rl
JOIN addresses ad ON ad.id = rl.address_id
JOIN accounts a ON a.id = ad.account_id
CROSS JOIN LATERAL (VALUES ('account:' || a.id), ('product:' || a.product_id), ('global'))
  AS k(scope_key)
WHERE rl.status = 'open' AND rl.consumed_by IS NULL AND rl.exposure_reserved
  AND k.scope_key = :'scope_key' \gset
\echo recomputed_minor=:recomputed_minor
UPDATE lock_exposure SET open_minor = :'recomputed_minor'::numeric, updated_at = now()
WHERE scope_key = :'scope_key'
RETURNING scope_key,open_minor::text AS after_minor;
COMMIT;
SQL
```

The simpler alternative, when a short outage is acceptable, is to stop the service, run the same
SQL, and start it again:

```sh
docker compose -f deploy/docker-compose.staging.yml stop topup
# Run the owner repair SQL above for each drifted scope key, then:
docker compose -f deploy/docker-compose.staging.yml start topup
```

An in-service repair path and a drift alert are follow-up work in
[#75](https://github.com/Phala-Network/crypto-topup-service/issues/75), extending
[#68](https://github.com/Phala-Network/crypto-topup-service/issues/68) item 3.

## Verification

Within a minute of recovery, the overdue-lock count is zero, `rate_lock.expired` events appear for
the drained locks, the ledger equals the recomputation, and no new `rate-lock expiry scan failed`
lines appear. Resume `quotes` if it was paused.

## Rollback

If a restart does not help, re-pause `quotes` and redeploy the retained prior compose hash. Expired
locks are never re-opened.
