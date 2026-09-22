# Operator key compromise exercise

Date: 2026-09-22. Environment: local `deploy/local` stack, project `wp-d5-exercise`.

```sh
# Signed POST /v1/admin/routes/phala-cloud-sepolia-pha-usd/pause
# body: {"scopes":["settlement","flush"]}
```

Observed:

```text
200 {"route":"phala-cloud-sepolia-pha-usd","paused_scopes":["flush","settlement"]}
flushes rows: 0
```

The local stack has no EVM or Safe. Role grant/revoke, nonce, and balance checks were marked
human-only and were not executed. Resume returned HTTP 200 with an empty scope list.
