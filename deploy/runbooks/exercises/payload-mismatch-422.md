# 422 payload mismatch exercise

Date: 2026-09-22.

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T \
  -e PGPASSWORD=topup postgres psql -h 127.0.0.1 -U topup_service -d topup \
  -c "BEGIN TRANSACTION READ ONLY; SELECT count(*) FROM settlements WHERE resend_forbidden; COMMIT;"
```

Observed count `0` and a successful read-only transaction. The route accepted and later removed the
`settlement` pause. A real HTTP 422 requires the product conformance fixture, not this local stack.
