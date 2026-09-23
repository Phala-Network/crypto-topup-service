# Valid runbook fixture

This file must pass `deploy/runbooks/check.sh`. It covers command shapes that are easy to
misclassify; it is not an operator runbook.

```sh
cargo test --locked -p topup --test refunds refund_flow -- --nocapture
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup restore-check
docker compose -f deploy/docker-compose.staging.yml exec -T postgres psql -c "SELECT 'topup bogus'"
topup reconcile \
  --route deploy/config/routes/phala-cloud-sepolia-pha.yaml
cargo run --locked -q -p topup -- route validate deploy/config/routes/phala-cloud-sepolia-pha.yaml
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
psql "$DATABASE_URL" <<'SQL'
topup bogus --not-a-command
SQL
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/body.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H "${headers[0]}" "$BASE_URL/v1/admin/deposits/$DEPOSIT_ID/nudge"
curl --fail-with-body -sS "$BASE_URL/v1/attestation?nonce=00"
```
