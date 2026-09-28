# Lock exposure near cap

**Trigger:** `TopupLockExposureNearCap` (a quote creation took the open lock credit of one
account in one mode, tag `scope:account` with the account's `acct_…` as `id`, or of every quote,
`scope:global`, to at least 90% of its cap; the event carries `open_minor` and `cap_minor`), or
merchants reporting `400 exposure_cap_exceeded`.

**Impact:** a creation that would exceed a cap answers `400 exposure_cap_exceeded`; existing
locks keep their terms until consumed, cancelled, or expired. The route's
`limits.max_open_minor` still uses the older key names: `account` caps one customer, `product` one
account in one mode, and `global` every open quote on every route (design §12 moves the first two
to per-account limits).

## First steps

1. Read `exposure_minor` (the global open lock credit) and each route's
   `open_rate_lock_exposure_atomic` in the daily report (`admin GET /v1/admin/reports/daily`); the
   caps are in the attested route (`topup route show`), `GET /v1/config` shows a merchant
   `max_open_amount_per_account`, which is the route's `account` key (the per-customer cap), and a quote refused by a cap
   (`400 exposure_cap_exceeded`) states the room left.
2. A lock whose window has closed keeps its reservation until the finalized chain passes
   `expires_at`, about 15 minutes later (architecture §9). If exposure does not fall after that,
   follow [lock expiry worker failure](lock-expiry-worker-failure.md).
3. To stop new quotes on the route while investigating:
   `admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'`.

## Decide

- Legitimate demand: the cap already rejects over-cap quotes; tell the merchant through its
  recorded contact and ask Finance and Risk whether to raise the caps.
- One customer concentrates exposure: pause `quotes` for that customer of the account
  in its mode
  (`admin POST "/v1/admin/accounts/$ACCOUNT/customers/$CUSTOMER/pause" '{"scopes":["quotes"],"livemode":true}'`),
  or pause the route.

## Fix

Exposure drains as locks are consumed, cancelled, or expired; never close locks by hand. A cap
change is a new route version and Deploy `upgrade`.

## Done when

The report shows exposure below 90% of every cap and `quotes` is resumed after Finance and Risk
approve.
