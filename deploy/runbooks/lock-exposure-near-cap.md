# Lock exposure near cap

**Trigger:** `TopupLockExposureNearCap` (a rate-lock creation took the open `product` or `global`
lock credit to at least 90% of the route's `limits.max_open_minor` cap; the event carries
`scope`, `open_minor`, and `cap_minor`), or products reporting `409 exposure_cap_exceeded`.

**Impact:** a creation that would exceed the `account`, `product`, or `global` cap answers `409`;
existing locks keep their terms until consumed, cancelled, or expired. `global` spans every
quote route.

## First steps

1. Read `exposure_minor` (the global open lock credit) and each route's
   `open_rate_lock_exposure_atomic` in the daily report (`admin GET /v1/admin/report/daily`); the
   caps are in the product-signed `GET /v1/config`, and a quote refused by a cap
   (`409 exposure_cap_exceeded`) states the room left.
2. A lock whose window has closed keeps its reservation until the finalized chain passes
   `expires_at`, about 15 minutes later (architecture §9). If exposure does not fall after that,
   follow [lock expiry worker failure](lock-expiry-worker-failure.md).
3. To stop new quotes on the route while investigating:
   `admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'`.

## Decide

- Legitimate demand: the cap already rejects over-cap quotes; tell the product and ask Finance
  and Risk whether to raise the caps.
- One customer concentrates exposure: pause `quotes` for that customer of the account
  (`admin POST "/v1/admin/accounts/$ACCOUNT/customers/$CUSTOMER/pause" '{"scopes":["quotes"]}'`),
  or pause the route.

## Fix

Exposure drains as locks are consumed, cancelled, or expired; never close locks by hand. A cap
change is a new route version and Deploy `upgrade`.

## Done when

The report shows exposure below 90% of every cap and `quotes` is resumed after Finance and Risk
approve.
