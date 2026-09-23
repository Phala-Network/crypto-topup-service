# Scanner lag exercise

Date: 2026-09-22.

Commands changed after this exercise (#95): `deploy/local/docker-compose.yml` is an overlay; pass
`-f deploy/docker-compose.yml -f deploy/local/docker-compose.yml`. The commands below are recorded
as run.

Status: partial; blocked on a controllable finalized-chain fixture and PR #56 alert rules.

G2 exercised once: [ ]

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml logs --no-color topup
```

Observed the scanner's retry path against the intentionally unavailable local RPC:

```text
finalized chain scan failed transiently
chain_id=11155111 error_category=chain_read retry_after_seconds=13
```

The read-only cursor query returned zero rows. The route accepted `quotes` and `addresses` pause
scopes as part of the all-scope exercise.
