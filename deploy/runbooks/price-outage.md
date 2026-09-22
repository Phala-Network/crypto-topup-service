# Price outage

## Trigger

Trigger on stale primary/check observations, source request failures, excessive price deviation,
FX guard failure, or stablecoin depeg. PR #56 metric and alert names are not on `main`.

## Impact and blast radius

Spot deposits remain `detected` and new rate locks must not be quoted. Existing on-chain funds are
safe and must not be manually credited.

## First 5 minutes

```sh
printf '%s' '{"scopes":["quotes"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=route="$ROUTE" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,state,attempt,next_attempt_at,valuation_at,price_scaled::text,quote,updated_at
FROM deposits WHERE route=:'route' AND state='detected' ORDER BY updated_at LIMIT 50;
COMMIT;
SQL
curl --fail-with-body -sS 'https://community-api.coinmetrics.io/v4/timeseries/asset-metrics?assets=pha&metrics=ReferenceRateUSD&frequency=1m&limit_per_asset=1&paging_from=end'
curl --fail-with-body -sS 'https://data-api.binance.vision/api/v3/ticker/price?symbol=PHAUSDT'
curl --fail-with-body -sS 'https://api.kraken.com/0/public/Ticker?pair=USDTUSD'
```

## Decision tree

- One source down: wait for recovery; do not weaken two-source validation.
- Sources reachable but divergent: keep quotes paused and investigate market integrity.
- All sources agree and are fresh: observe for two policy windows, then resume.

## Remediation

No runtime source override exists. **HUMAN-ONLY:** a source/config change requires a new route
version and compose hash through the D2 upgrade flow. Never insert a price or credit directly.

## Verification

New deposits store fresh `valuation_at`, expected `price_scaled`, and evidence naming both sources.
Resume `quotes` with the signed curl pattern against `/v1/admin/routes/$ROUTE/resume`.

## Rollback

Re-pause quotes and redeploy the prior attested route version if it still has healthy sources.
