# Lock exposure near cap

## Trigger

Trigger on `TopupLockExposureNearCap` (a rate-lock creation took the open product or global lock
exposure to at least 90% of the route's cap; account scopes do not alert), on product reports of
`409 exposure_cap_exceeded` from rate-lock creation, or when the daily report shows unexpected open
lock exposure.

## Impact and blast radius

C10 checks each new lock's `credit_minor` against the open reserved locks of three scopes:
`account:<account_id>`, `product:<product_id>`, and `global`. A creation that would exceed any cap
answers `409 exposure_cap_exceeded`; existing locks keep their terms until they are consumed,
cancelled, or expired. The `global` scope spans every quote-first route. Persistent-address
deposits are unaffected.

## First 5 minutes

Read the caps from the attested route file (`rate_lock.max_open_minor`); when several enabled
quote-first routes differ, use the smallest value for each scope. Then read the open reservations
with the application role:

```sh
grep -E '^[[:space:]]+max_open_minor:' "$ROUTE_FILE"
export ACCOUNT_CAP_MINOR=500000 PRODUCT_CAP_MINOR=5000000 GLOBAL_CAP_MINOR=10000000
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=account_cap="$ACCOUNT_CAP_MINOR" \
  --set=product_cap="$PRODUCT_CAP_MINOR" --set=global_cap="$GLOBAL_CAP_MINOR" <<'SQL'
BEGIN TRANSACTION READ ONLY;
WITH caps(scope, cap_minor) AS (
  VALUES ('account', :'account_cap'::numeric), ('product', :'product_cap'::numeric),
         ('global', :'global_cap'::numeric)
), reserved AS (
  SELECT k.scope_key, sum(rl.credit_minor) AS reserved_minor,
         sum(rl.credit_minor) FILTER (WHERE rl.expires_at > now()) AS unexpired_minor,
         count(*) FILTER (WHERE rl.expires_at < c.scanned_block_time AND NOT EXISTS (
           SELECT 1 FROM deposits d
           WHERE d.address_id = rl.address_id AND d.state = 'detected'
             AND d.block_time <= rl.expires_at)) AS overdue_locks
  FROM rate_locks rl
  JOIN addresses ad ON ad.id = rl.address_id
  JOIN accounts a ON a.id = ad.account_id
  LEFT JOIN cursors c ON c.chain_id = ad.chain_id
  CROSS JOIN LATERAL (VALUES ('account:' || a.id), ('product:' || a.product_id), ('global'))
    AS k(scope_key)
  WHERE rl.status = 'open' AND rl.exposure_reserved
  GROUP BY k.scope_key
)
SELECT r.scope_key,
       r.reserved_minor::text AS open_reserved_minor,
       coalesce(r.unexpired_minor, 0)::text AS unexpired_minor,
       r.overdue_locks,
       c.cap_minor::text AS cap_minor,
       (r.reserved_minor * 10000 / c.cap_minor)::bigint AS bps_of_cap
FROM reserved r
JOIN caps c ON c.scope = split_part(r.scope_key, ':', 1)
ORDER BY bps_of_cap DESC, r.scope_key;
COMMIT;
SQL
```

`open_reserved_minor` is what C10 enforces each cap against. `unexpired_minor` excludes locks whose payment window has closed;
those keep their reservation until the finalized chain passes `expires_at`, about 15 minutes later
(architecture §9). `overdue_locks` counts locks the expiry worker could already expire; a non-zero
count older than a few expiry scans means exposure is not being released.

If new quotes must stop for the whole route while the cause is investigated, pause `quotes`:

```sh
printf '%s' '{"scopes":["quotes"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

## Decision tree

- Legitimate demand: C10 already rejects over-cap quotes; leave quotes running, tell the product,
  and ask Finance/Risk whether to raise caps.
- One account concentrates exposure unexpectedly: ask the product to pause `quotes` for that
  account (`POST /v1/products/{p}/accounts/{ext}/pause`, product-signed), or pause the route as
  above, and investigate the tenant.
- `overdue_locks > 0` persists: follow [Lock expiry worker failure](lock-expiry-worker-failure.md).

## Remediation

Exposure drains as locks are consumed, cancelled by the product, or expired by the worker. The
service never needs a manual write to `rate_locks` for a legitimate near-cap condition. A cap change
is attested route configuration: create a new route version, run `topup route validate`, render a
new compose hash, and run Deploy in mode `upgrade` ([deploy/README.md, "Deploy"](../README.md#deploy)).

## Verification

Re-run the query: every scope is below 90% of its cap, `overdue_locks` is zero, and `TopupLockExposureNearCap` has resolved. Existing
locks retain their original terms. Resume `quotes` only after Finance/Risk approval.

## Rollback

Re-pause `quotes` and deploy the prior cap version. Existing locks are never repriced or cancelled by
rollback.
