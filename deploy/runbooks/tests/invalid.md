# Invalid runbook fixture

This file must fail `deploy/runbooks/check.sh` with exactly the errors the script expects; it is
not an operator runbook.

```sh
topup bogus
topup route bogus deploy/config/routes/phala-cloud-sepolia-pha.yaml
topup outbox
topup attest --bogus
topup reconcile \
  --no-such-flag --route deploy/config/routes/phala-cloud-sepolia-pha.yaml
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup restore-check --once
cargo run --locked -q -p topup -- restore
curl -sS -X POST "$BASE_URL/v1/admin/not-a-route"
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh GET "$BASE_URL/v1/admin/routes/r/pause" /tmp/empty "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl -sS -X DELETE "$BASE_URL/v1/admin/report/daily"
```
