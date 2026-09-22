# Lock exposure near cap

## Trigger

Trigger when account, product, or global open rate-lock exposure approaches its configured cap.
PR #56 alert names and C10 reservation behavior are not on `main`.

## Impact and blast radius

New quotes may be rejected while existing locks remain valid until expiry/consumption. Blast radius
depends on which cap is near exhaustion.

## First 5 minutes

```sh
printf '%s' '{"scopes":["quotes"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=account_cap=500000 --set=product_cap=5000000 --set=global_cap=10000000 <<'SQL'
BEGIN TRANSACTION READ ONLY;
WITH open_locks AS (
  SELECT rl.route,a.id AS account_id,p.id AS product_id,rl.credit_minor
  FROM rate_locks rl
  JOIN addresses ad ON ad.id=rl.address_id
  JOIN accounts a ON a.id=ad.account_id
  JOIN products p ON p.id=a.product_id
  WHERE rl.consumed_by IS NULL AND rl.expires_at>now()
), exposure AS (
  SELECT route,account_id,product_id,
         sum(credit_minor) OVER (PARTITION BY route,account_id) AS account_open_minor,
         sum(credit_minor) OVER (PARTITION BY route,product_id) AS product_open_minor,
         sum(credit_minor) OVER (PARTITION BY route) AS global_open_minor
  FROM open_locks
)
SELECT route,account_id,product_id,
       max(account_open_minor)::text AS account_open_minor,
       :'account_cap' AS account_cap,
       max(product_open_minor)::text AS product_open_minor,
       :'product_cap' AS product_cap,
       max(global_open_minor)::text AS global_open_minor,
       :'global_cap' AS global_cap
FROM exposure
GROUP BY route,account_id,product_id
ORDER BY route,product_id,account_id;
COMMIT;
SQL
```

## Decision tree

- Legitimate demand near approved cap: keep quotes paused until locks expire or Finance raises caps.
- Unexpected concentration/account abuse: keep scoped pause and investigate the account/product.
- Exposure query unavailable/inconsistent: treat as reconciliation issue and do not raise caps.

## Remediation

Cap changes are attested route configuration: create a new route version, run `topup route
validate`, render a new compose hash, and use the D2 Safe-approved upgrade flow. C10 is not on
`main`; do not claim reservation enforcement until it merges and is exercised.

## Verification

Open exposure is below the approved threshold, existing locks retain original terms, and new quotes
reserve exposure atomically. Resume quotes only after Finance/Risk approval.

## Rollback

Re-pause quotes and deploy the prior cap version. Existing locks are never repriced or cancelled by
rollback.
