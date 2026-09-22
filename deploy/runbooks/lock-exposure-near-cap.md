# Lock exposure near cap

## Trigger

Trigger on `TopupLockExposureNearCap` (PR #56: `topup_open_lock_exposure_minor` at or above 90% of
`topup_open_lock_exposure_cap_minor` for five minutes), on product reports of
`409 exposure_cap_exceeded` from rate-lock creation, or when the daily report shows unexpected open
lock exposure.

## Impact and blast radius

C10 reserves each lock's `credit_minor` atomically against three counters in `lock_exposure`:
`account:<account_id>`, `product:<product_id>`, and `global`. A creation that would exceed any cap
answers `409 exposure_cap_exceeded`; existing locks keep their terms until they are consumed,
cancelled, or expired. The `global` counter spans every quote-first route. Persistent-address
deposits are unaffected.

## First 5 minutes

Read the caps from the attested route file (`rate_lock.max_open_minor`); when several enabled
quote-first routes differ, use the smallest value for each scope. Then read the ledger and the
recomputed reservations with the application role:

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
         count(*) FILTER (WHERE rl.expires_at <= now()) AS overdue_locks
  FROM rate_locks rl
  JOIN addresses ad ON ad.id = rl.address_id
  JOIN accounts a ON a.id = ad.account_id
  CROSS JOIN LATERAL (VALUES ('account:' || a.id), ('product:' || a.product_id), ('global'))
    AS k(scope_key)
  WHERE rl.status = 'open' AND rl.consumed_by IS NULL AND rl.exposure_reserved
  GROUP BY k.scope_key
)
SELECT scope_key,
       coalesce(l.open_minor, 0)::text AS ledger_open_minor,
       coalesce(r.reserved_minor, 0)::text AS open_reserved_minor,
       coalesce(r.unexpired_minor, 0)::text AS unexpired_minor,
       coalesce(r.overdue_locks, 0) AS overdue_locks,
       c.cap_minor::text AS cap_minor,
       (coalesce(l.open_minor, 0) * 10000 / c.cap_minor)::bigint AS ledger_bps_of_cap
FROM lock_exposure l
FULL JOIN reserved r USING (scope_key)
JOIN caps c ON c.scope = split_part(scope_key, ':', 1)
WHERE coalesce(l.open_minor, 0) > 0 OR r.scope_key IS NOT NULL
ORDER BY ledger_bps_of_cap DESC, scope_key;
COMMIT;
SQL
```

`ledger_open_minor` is what C10 enforces. `open_reserved_minor` recomputes it from open, unconsumed,
reserved locks and must be equal. `unexpired_minor` excludes locks already past `expires_at`; a
non-zero `overdue_locks` count older than a few expiry scans means exposure is not being released.

If new quotes must stop for the whole route while the cause is investigated, pause `quotes`:

```sh
printf '%s' '{"scopes":["quotes"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

## Decision tree

- Ledger equals recomputation, legitimate demand: C10 already rejects over-cap quotes; leave quotes
  running, tell the product, and ask Finance/Risk whether to raise caps.
- One account concentrates exposure unexpectedly: ask the product to pause `quotes` for that
  account (`POST /v1/products/{p}/accounts/{ext}/pause`, product-signed), or pause the route as
  above, and investigate the tenant.
- `overdue_locks > 0` persists: follow [Lock expiry worker failure](lock-expiry-worker-failure.md).
- Ledger differs from recomputation: treat it as a reconciliation incident, pause `quotes`, and do
  not raise caps. The only repair is the owner procedure in
  [Lock expiry worker failure](lock-expiry-worker-failure.md).

## Remediation

Exposure drains as locks are consumed, cancelled by the product, or expired by the worker. The
service never needs a manual write to `lock_exposure` or `rate_locks` for a legitimate near-cap
condition. A cap change is attested route configuration:
create a new route version, run `topup route validate`, render a new compose hash, and use the D2
Safe-approved upgrade flow.

## Verification

Re-run the query: every scope is below 90% of its cap, `ledger_open_minor` equals
`open_reserved_minor`, `overdue_locks` is zero, and `TopupLockExposureNearCap` has resolved. Existing
locks retain their original terms. Resume `quotes` only after Finance/Risk approval.

## Rollback

Re-pause `quotes` and deploy the prior cap version. Existing locks are never repriced or cancelled by
rollback.
