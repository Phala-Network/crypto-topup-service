# Lock exposure near cap

**Trigger:** `TopupLockExposureNearCap` (a quote creation took the open lock credit of one
account in one mode to at least 90% of its cap; tag `scope:account` with the account's `acct_…`
as `id` and `livemode`, and the event carries `open_minor` and `cap_minor`), or merchants
reporting `400 exposure_cap_exceeded`.

**Impact:** a creation that would exceed a cap answers `400 exposure_cap_exceeded`; existing
locks keep their terms until consumed, cancelled, or expired. The caps are per account and mode
only (design §12), in `account_limits`: open quotes (default 1 000 live, 100 test), their credit
(default $50 000 live, $10 000 test), and one customer's credit (default $5 000). There is no
global cap, and test-mode quotes never use live headroom, so one account's demand never blocks
another's.

## First steps

1. Read the account's effective caps: the `limits` of the admin account response (any
   `admin POST "/v1/admin/accounts/$ACCOUNT"` answers them), or `GET /v1/config` with the
   merchant's key (`max_open_quotes`, `max_open_amount_per_account`,
   `max_open_amount_per_customer`). A quote refused by a cap (`400 exposure_cap_exceeded`) states
   the room left. The daily report (`admin GET /v1/admin/reports/daily`) shows each route's
   `open_rate_lock_exposure_atomic`.
2. A lock whose window has closed keeps its reservation until the finalized chain passes
   `expires_at`, about 15 minutes later (architecture §9). If exposure does not fall after that,
   follow [lock expiry worker failure](lock-expiry-worker-failure.md).
3. To stop new quotes on the route while investigating:
   `admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'`.

## Decide

- Legitimate demand: the cap already rejects over-cap quotes; tell the merchant through its
  recorded contact and ask Finance and Risk whether to raise the account's caps.
- One customer concentrates exposure: pause `quotes` for that customer of the account
  in its mode
  (`admin POST "/v1/admin/accounts/$ACCOUNT/customers/$CUSTOMER/pause" '{"scopes":["quotes"],"livemode":true}'`),
  or pause the route.

## Fix

Exposure drains as locks are consumed, cancelled, or expired; never close locks by hand. After
Finance and Risk approve, raise the account's caps in one mode (audited, and announced to the
account as `account.updated`):
`admin POST "/v1/admin/accounts/$ACCOUNT" '{"limits":{"livemode":true,"max_open_amount_per_account":10000000},"reason":"approved by Finance and Risk"}'`.

## Done when

The account's open credit is below 90% of its caps and `quotes` is resumed after Finance and
Risk approve.
