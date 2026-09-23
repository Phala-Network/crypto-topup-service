# Route or chain retirement

## Trigger

Trigger before removing a route version from the attested compose, or before removing a chain's
last route (and with it that chain's scanner). Keeping an old version enabled for historical
deposits, as in [Treasury change](treasury-change.md), is not a retirement.

## Impact and blast radius

The service only scans chains that have a loaded route. A rate lock expires only once its chain's
scanner has committed through a finalized block past `expires_at` (architecture §9), so a lock left
open on a chain without a scanner never expires: its exposure stays reserved against the account,
product, and global caps, and the product never receives `rate_lock.expired`. A deposit still in
flight on that chain also stops progressing. Retiring a version whose chain keeps another route
does not stop the scanner, but its in-flight deposits still need their version.

## First 5 minutes

Stop new quotes on the route, then list what still depends on it with the application role:

```sh
printf '%s' '{"scopes":["quotes","addresses"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT rl.route, count(*) AS open_locks, max(rl.expires_at) AS last_window_close
FROM rate_locks rl
JOIN addresses ad ON ad.id = rl.address_id
WHERE rl.status = 'open' AND (rl.route = :'route' OR ad.chain_id = :chain_id)
GROUP BY rl.route;
SELECT route, route_version, state, count(*)
FROM deposits
WHERE state NOT IN ('swept', 'rejected') AND (route = :'route' OR chain_id = :chain_id)
GROUP BY route, route_version, state ORDER BY route, route_version, state;
COMMIT;
SQL
```

## Decision tree

- No open locks and no in-flight deposits for the route (or, for a chain, on that chain): continue
  with Remediation.
- Open locks remain: keep the route and its chain loaded until every lock is consumed, cancelled by
  the product before its window closes, or expired by the worker. With `quotes` paused no new locks
  appear, so this drains within the lock window plus finality, about 15 minutes after
  `last_window_close`. If locks stay open longer, follow
  [Lock expiry worker failure](lock-expiry-worker-failure.md) and [Scanner lag](scanner-lag.md);
  never close locks by hand.
- In-flight deposits remain: let them reach `credited` and `swept` (or `rejected`) first; follow
  the runbook their stuck state points to.

## Remediation

Only after both queries return no rows for the route (and, when retiring a chain, for the chain):
remove the route version, or the chain's last route, create the new compose, run
`deploy/validate-compose.sh` and `deploy/render-compose.sh`, and use the D2 Safe-approved upgrade
flow. Retired lock and persistent addresses on a chain that stays loaded remain monitored.

## Verification

Re-run the queries after the upgrade: still no rows. The daily report shows no open locks or
exposure for the retired route, and `TopupLockExposureNearCap` exposure has not grown.

## Rollback

Redeploy the retained prior compose hash; the route, its scanner, and its locks resume from
database state. Resume `quotes` and `addresses` with the signed `resume` request only if the
retirement is abandoned.
