# Price outage exercise

Date: 2026-09-22. Authenticated route pause was exercised locally with the all-scope body, which
includes `quotes`.

```text
200 {"route":"phala-cloud-sepolia-pha-usd","paused_scopes":["addresses","flush","quotes","refunds","settlement"]}
deposits rows: 0
```

External price APIs were not called from the exercise; the local stack has no price-source
simulator or deposits to value. Resume returned an empty scope list.
