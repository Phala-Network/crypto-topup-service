# Treasury change

## Trigger

Trigger only for an approved treasury migration or when the current treasury Safe is no longer
acceptable. There is no runtime treasury setter.

## Impact and blast radius

Every existing forwarder is immutably bound to the old treasury through its implementation.
Changing treasury requires a new factory and route version; old route versions remain needed for
historical deposits.

## First 5 minutes

For an emergency migration, pause creation and movement on the route:

```sh
printf '%s' '{"scopes":["quotes","addresses","settlement","flush","refunds"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
cast call "$FACTORY" 'implementation()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$IMPLEMENTATION" 'treasury()(address)' --rpc-url "$RPC_PROVIDER_A_URL"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=route="$ROUTE" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT route,route_version,state,count(*) FROM deposits WHERE route=:'route'
GROUP BY route,route_version,state ORDER BY route_version,state;
COMMIT;
SQL
```

## Decision tree

- Current Safe remains secure: use a planned migration window.
- Current Safe compromised: keep all scopes paused and invoke the Safe incident procedure.
- Deterministic factory tooling or audit evidence missing: stop; do not ad hoc deploy.

## Remediation

**HUMAN-ONLY:** deploy a new `ForwarderFactory(FINANCE_SAFE, NEW_TREASURY)` using the audited
deterministic A2 procedure, grant the attested operator role, and record factory, implementation,
treasury, code hashes, and transaction hashes. `main` has no deterministic deployment command;
this is a blocking gap.

Create a new route version with the new factory/implementation/treasury, retain the old version,
then validate and render a new compose hash:

```sh
topup route validate deploy/config/routes/phala-cloud-sepolia-pha.yaml
export TOPUP_IMAGE=ghcr.io/phala-network/crypto-topup@sha256:<digest>
export POSTGRES_WALG_IMAGE=ghcr.io/phala-network/postgres-walg@sha256:<digest>
deploy/render-compose.sh > deploy/docker-compose.staging.yml
deploy/validate-compose.sh
docker compose -f deploy/docker-compose.staging.yml config >/dev/null
```

**HUMAN-ONLY:** approve the exact prepared compose hash through the Finance Safe and use the D2
upgrade flow.

## Verification

Verify both providers return the new code, `implementation()`, `treasury()`, operator role, sample
`addressOf(bytes32)`, attested compose, and route version. Exercise one small Sepolia deposit and
flush before resuming scopes.

## Rollback

Re-pause all scopes and redeploy the retained old compose hash. New-version addresses cannot be
rebound; continue monitoring both route versions and never delete the new factory record.
