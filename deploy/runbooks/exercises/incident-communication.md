# Incident communication exercise

Date: 2026-09-22.

Commands changed after this exercise (#95): `deploy/local/docker-compose.yml` is an overlay; pass
`-f deploy/docker-compose.yml -f deploy/local/docker-compose.yml`. The commands below are recorded
as run.

Status: partial; blocked on human status-page publication and incident-role assignment.

G2 exercised once: [ ]

```sh
curl -sS -o /dev/null -w '%{http_code}' http://127.0.0.1:18085/healthz
docker compose -p wp-d5-exercise -f deploy/local/docker-compose.yml exec -T topup \
  topup attest --nonce deadbeef
```

Observed HTTP `200`; attestation returned `keyid=settlement/v1`, a 64-character public key, and a
non-empty simulator quote. The state snapshot showed every business table empty and all pause scopes
resumed. Status-page publication and role assignment are human-only and were not executed.
