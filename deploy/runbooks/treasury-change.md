# Treasury change

## Trigger

Trigger only for an approved treasury migration or when the current treasury Safe is no longer
acceptable. There is no runtime treasury setter.

## Impact and blast radius

Every existing forwarder is immutably bound to the old treasury through its implementation.
Changing treasury requires a new factory and route version; old route versions remain needed for
historical deposits.

## First 5 minutes

For an emergency migration, pause every scope on the route. Since C7b (#70) the `flush` scope stops
new sends: since #71 each unsigned plan of the route is voided when the sender reaches it, with one
`flush.send_paused` audit record naming the paused level, while other routes on the chain keep
sending and an already broadcast transaction still confirms. Snapshot the route's `sent` ids after
the pause returns HTTP 200, then verify none were added:

```sh
printf '%s' '{"scopes":["quotes","addresses","settlement","flush","refunds"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' AND receipt->'binding'->>'route'=:'route' ORDER BY id; COMMIT;" > /tmp/sent-before-pause
cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' AND receipt->'binding'->>'route'=:'route' ORDER BY id; COMMIT;" > /tmp/sent-after-pause
comm -13 /tmp/sent-before-pause /tmp/sent-after-pause > /tmp/new-sent-after-pause
test ! -s /tmp/new-sent-after-pause
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=route="$ROUTE" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT route,route_version,state,count(*) FROM deposits WHERE route=:'route'
GROUP BY route,route_version,state ORDER BY route_version,state;
SELECT max(created_at) AS paused_at FROM audit
WHERE action='pause' AND subject='route:' || :'route';
SELECT subject AS flush_id,reason,created_at FROM audit
WHERE action='flush.send_paused' AND created_at >= (
  SELECT max(created_at) FROM audit WHERE action='pause' AND subject='route:' || :'route'
) ORDER BY created_at;
COMMIT;
SQL
```

If the current Safe or operator is suspected compromised, the Finance Safe revoking the operator
role is the hard stop because it does not depend on the service:

**HUMAN-ONLY, Finance Safe, when a chain-level stop is required:**

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

## Decision tree

- Current Safe remains secure: use a planned migration window.
- Current Safe compromised: keep all scopes paused and invoke the Safe incident procedure.
- Safe verification, deployment verification, or audit evidence fails: stop; do not ad hoc deploy.

## Remediation

**HUMAN-ONLY:** follow the A2 procedure in [`deploy/CONTRACTS.md`](../CONTRACTS.md). The factory
address commits to `admin` and `treasury`, so a new treasury always yields a new factory. Update
`treasury` in `deploy/contracts/safe-expectations.json` through a reviewed PR, load `PRIVATE_KEY`
interactively as that document shows, then verify every Safe, dry-run, broadcast, and verify the
deployment through both providers. `NETWORK` is the network name in that file (`sepolia` or
`mainnet`):

```sh
export NETWORK=sepolia
export ADMIN="$FINANCE_SAFE"
export TREASURY="$NEW_TREASURY"
deploy/contracts/verify-safe.sh --rpc "$NETWORK"/a="$RPC_PROVIDER_A_URL" --rpc "$NETWORK"/b="$RPC_PROVIDER_B_URL"
deploy/contracts/deploy-factory.sh --rpc "$NETWORK"/a="$RPC_PROVIDER_A_URL" --dry-run
deploy/contracts/deploy-factory.sh --rpc "$NETWORK"/a="$RPC_PROVIDER_A_URL" --broadcast
deploy/contracts/verify-deployment.sh --rpc "$NETWORK"/a="$RPC_PROVIDER_A_URL" --rpc "$NETWORK"/b="$RPC_PROVIDER_B_URL" > treasury-change-verification.json
jq -e '.passed == true' treasury-change-verification.json
```

Then grant the attested operator role on the new factory through the Safe and record factory,
implementation, treasury, code hashes, and transaction hashes.

Create a new route version with the new factory/implementation/treasury, retain the old version,
then validate and render a new compose hash. `$NEW_ROUTE_FILE` is the new version's file; the
committed Sepolia file is a template and needs `--template` until it carries real addresses:

```sh
topup route validate "$NEW_ROUTE_FILE"
export TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:<digest>
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:<digest>
# Export the attested settings first: the `staging` Environment variables
# (deploy/README.md, "Attested settings").
deploy/render-compose.sh > deploy/docker-compose.staging.yml
deploy/validate-compose.sh
docker compose -f deploy/docker-compose.staging.yml config >/dev/null
```

**HUMAN-ONLY:** approve the exact prepared compose hash through the Finance Safe and use the D2
upgrade flow.

## Verification

Verify both providers return the new code, `implementation()`, `treasury()`, operator role, sample
`addressOf(bytes32)`, attested compose, and route version. Exercise one small Sepolia deposit and
flush before resuming scopes. If the old role was revoked, the Safe grants only the attested
operator on the new factory. Resume the paused scopes with the signed `resume` request only after
the new route version is live.

## Rollback

Re-pause the route, revoke the new factory's operator role if required, and redeploy the retained old
compose hash. New-version addresses cannot be rebound; continue monitoring both route versions and
never delete the new factory record.
