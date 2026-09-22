# Provider disagreement exercise

Date: 2026-09-22. The local compose intentionally points both RPC variables at
`http://127.0.0.1:1`.

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml logs --no-color topup
```

Observed repeated bounded retry evidence:

```text
finalized chain scan failed transiently
chain_id=11155111 error_category=chain_read retry_after_seconds=13
```

The read-only deposits query returned zero rows. Finalized-block comparison was not feasible because
the local stack has no EVM provider.
