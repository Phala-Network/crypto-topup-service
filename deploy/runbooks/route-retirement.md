# Route or chain retirement

**Trigger:** before removing a route version from the attested compose, or a chain's last route
(and with it the chain's scanner). Keeping an old version loaded for its deposits, as after a
[treasury change](treasury-change.md), is not a retirement.

**Impact:** a chain is scanned only while it has a loaded route, and a rate lock expires only once
its chain's scanner passes `expires_at` (architecture §9). A lock left open on a chain without a
scanner never expires: its exposure stays reserved and the product never gets `rate_lock.expired`.
In-flight deposits on the chain stop too, and deposits of a removed version lose their version.

## First steps

Stop new locks and addresses on the route:

```sh
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'
```

Then, in the daily report (`admin GET /v1/admin/report/daily`), read the route's (for a chain,
every route on it) `open_rate_lock_exposure_atomic`, the `detected`, `confirmed`, and `credited` counts of
`deposits_by_state`, and `credited_undelivered`.

## Decide

- Open lock exposure: wait until every lock is consumed, cancelled, or expired, about 15 minutes
  after the last window closes; if it stays, follow
  [lock expiry worker failure](lock-expiry-worker-failure.md) and [scanner lag](scanner-lag.md).
- In-flight deposits: let them reach `swept` or `rejected`, following the runbook their alert
  points to.

## Fix

Only when the open exposure and every in-flight count are zero: remove the route version (or the
chain's last route) by reviewed PR (`deploy/validate-compose.sh` passes) and Deploy `upgrade`.

## Done when

The report no longer lists the retired route, and the remaining routes show no exposure growth.
Rollback is a Deploy `upgrade` to the prior compose: the route, its scanner, and its locks resume
from the database; resume `quotes` only if the retirement is abandoned.
