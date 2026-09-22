# Treasury change exercise

Date: 2026-09-22.

```sh
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T topup \
  topup route validate /etc/topup/routes/phala-cloud-sepolia-pha.yaml
```

Observed:

```text
route file `/etc/topup/routes/phala-cloud-sepolia-pha.yaml` is valid at schema level;
on-chain deployment and Safe control were not checked
```

All five pause scopes were exercised. New factory deployment, Safe role grant, compose allow-list,
and chain verification are human-only and were not run because no local EVM/Safe exists.
