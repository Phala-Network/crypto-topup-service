# Price outage

**Trigger:** `TopupDepositStateAgeExceeded` with `state:detected` whose latest timeline evidence
is `stage: "valuation"` ([provider disagreement](provider-disagreement.md), step 1): a stale or
unavailable source, a primary/check deviation, an FX guard failure, or a stablecoin depeg.

**Impact:** spot deposits stay `detected` and rate locks cannot be priced. Funds on chain are safe
and must never be credited by hand.

## First steps

Stop new quotes, then check the sources the route uses:

```sh
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes"]}'
curl --fail-with-body -sS 'https://community-api.coinmetrics.io/v4/timeseries/asset-metrics?assets=pha&metrics=ReferenceRateUSD&frequency=1m&limit_per_asset=1&paging_from=end'
curl --fail-with-body -sS 'https://data-api.binance.vision/api/v3/ticker/price?symbol=PHAUSDT'
curl --fail-with-body -sS 'https://api.kraken.com/0/public/Ticker?pair=USDTUSD'
```

## Decide

- One source down: wait; never weaken the two-source check.
- Sources reachable but divergent: keep quotes paused and investigate market integrity.
- All fresh and in agreement: watch two policy windows, then resume.

## Fix

There is no runtime price override. Changing a source is a route config change and Deploy
`upgrade`.

## Done when

New deposits leave `detected` with a fresh valuation (the admin deposit view) and quotes are resumed:
`admin POST "/v1/admin/routes/$ROUTE/resume" '{"scopes":["quotes"]}'`.
