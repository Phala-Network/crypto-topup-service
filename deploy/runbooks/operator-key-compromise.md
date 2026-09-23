# Operator key compromise

## Trigger

Trigger on an operator transaction not linked to a `flushes` row, an unexpected pending nonce,
`MissingConsumedReceipt`, or evidence that operator key material was exposed. PR #56 defines no
dedicated alert; an unexplained drop behind `TopupOperatorGasReserveLow` is a common first signal.

## Impact and blast radius

The operator can spend only its gas and call `ForwarderFactory.flush`; funds can move only to the
immutable treasury. Blast radius is unnecessary gas spend, forced flush timing, and every factory
where the compromised address has `OPERATOR_ROLE`.

## Emergency: confirmed compromise

The Finance Safe revokes the compromised role **before any other action**. The compromised key is
never re-authorized, including as rollback.

### First 5 minutes

**HUMAN-ONLY, Finance Safe:** derive the role and execute the exact revoke calldata through the
Safe for every affected factory, then verify the finalized result through both providers:

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

Both calls must return `false`; do not begin any other remediation before the Safe revocation is
executed. Then pause the `flush` scope on every route of the chain so the service stops trying to
send with the revoked key, and confirm that no new flush row became `sent` after the pause (repeat
the signed request per route):

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-before-pause
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-after-pause
comm -13 /tmp/sent-before-pause /tmp/sent-after-pause > /tmp/new-sent-after-pause
test ! -s /tmp/new-sent-after-pause
```

The pause only stops this service; the Safe revocation is what stops anyone else holding the key.
Deposits keep being detected, settled, and credited while flushing is paused.

After the revocation, the chain's running flusher logs `flusher paused: the configured operator does
not hold OPERATOR_ROLE` and raises the `OperatorRoleMissing` flusher alert at every maintenance
interval (5 seconds) until the replacement version is deployed. This is
expected: it plans and sends nothing and keeps maintaining already sent flushes.

Stop the service container only if the CVM itself is suspected of leaking the key. That halts
detection, settlement, and crediting on **every route** until a replacement is running, and every
`operator/v{n}` of the same application is equally exposed (see Remediation):

```sh
docker compose -f deploy/docker-compose.staging.yml stop topup
```

Preserve evidence and assess the affected nonces:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,operator,nonce::text,status,tx_hash,receipt FROM flushes
WHERE chain_id=:chain_id ORDER BY nonce DESC LIMIT 20;
COMMIT;
SQL
cast nonce "$OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

### Remediation

**HUMAN-ONLY:** bring the next operator key version, `operator/v{n+1}`, into service. The attested
service derives it; never grant an arbitrary EOA as a workaround. Every version derives from the
same dstack application key, so if the CVM or application key itself is suspected, a new version is
equally exposed: escalate to Security for a new application identity instead.

1. Read the new operator address from the running deployment (dstack derives keys from the
   application identity, not the compose hash) and export the printed address as
   `NEW_OPERATOR_ADDRESS`:

```sh
export NEW_OPERATOR_KEY_VERSION=2
export NONCE="$(openssl rand -hex 32)"
docker compose -f deploy/docker-compose.staging.yml exec -T topup topup attest --nonce "$NONCE" --operator-key-version "$NEW_OPERATOR_KEY_VERSION" | jq -r .operator_address
```

2. **HUMAN-ONLY, Finance Safe:** grant the new operator on every affected factory and verify the
   finalized result through both providers:

```sh
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

3. Fund the new operator with native gas on every affected chain through [Gas refill](gas-refill.md):

```sh
cast balance "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

4. Set `chain.operator_key_version: $NEW_OPERATOR_KEY_VERSION` in a new `version` of every current
   route file on the chain (current routes on one chain must share it),
   validate, then upgrade through the attested path in [deploy/README.md](../README.md): new
   compose hash, allow-list, deploy.

```sh
topup route validate "$ROUTE_FILE"
deploy/validate-compose.sh
```

5. Confirm one `flusher operator holds OPERATOR_ROLE` log line per chain/token route with the new
   `operator_key_version` and `operator`. Until the grant is visible, the new flusher keeps
   raising `OperatorRoleMissing` and sends nothing.

```sh
docker compose -f deploy/docker-compose.staging.yml logs topup | grep 'flusher operator holds OPERATOR_ROLE'
```

C7 re-binds stale `planned` rows to the new operator and its own nonce sequence, which starts at 0,
at the next scheduled planning run (`chain.flush.schedule`), including while `flush` is paused.
Verify the rows and append-only audit:

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=new_operator="$NEW_OPERATOR_ADDRESS" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,operator,nonce::text,status FROM flushes
WHERE chain_id=:chain_id AND status='planned' ORDER BY id;
SELECT subject,actor,reason,created_at FROM audit
WHERE action='flush.plan_operator_rebound' ORDER BY created_at DESC LIMIT 20;
SELECT count(*) AS stale_plans FROM flushes
WHERE chain_id=:chain_id AND status='planned' AND operator<>:'new_operator';
COMMIT;
SQL
```

Require `stale_plans=0`, the expected new operator on every planned row, and one matching
`flush.plan_operator_rebound` audit record per re-bound plan before resuming `flush`.

### Verification

Require the old role to remain false on both providers, the new role to be true, no unexpected sent
row after the pause, no stale old-operator plans, and expected rebound audits.

## Routine rotation

For a scheduled rotation with no compromise, use the architecture's short overlap procedure:

1. Read the next operator address with `topup attest --operator-key-version <n+1>` (Remediation
   step 1).
2. **HUMAN-ONLY:** grant it `OPERATOR_ROLE` through the Finance Safe.
3. Fund it with native gas on every chain.
4. Bump `chain.operator_key_version` in every current route file, then deploy the new
   compose hash (Remediation step 4).
5. Verify the `flusher operator holds OPERATOR_ROLE` log line, both roles, the new operator nonce,
   one successful flush, and any C7 plan-rebind audits.
6. **HUMAN-ONLY:** revoke the old role through the Finance Safe once the new service is healthy and
   no flush of the old operator is `sent`.

```sh
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS"
cast nonce "$NEW_OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
```

## Decision tree

- Confirmed exposure or unknown operator transaction: use the emergency path; revoke first.
- Only a known replacement transaction: verify its hash history in `receipt`; continue monitoring.
- Provider A alone reports the transaction: follow provider disagreement before declaring exposure.
- Scheduled, controlled key change with no exposure: use routine rotation.

## Verification

For compromise, require the old role to remain false on both providers, the new role to be true,
no unexpected sent row after the pause, no stale old-operator plans, and expected rebound audits.
For routine rotation, additionally require a confirmed flush from the new operator before revoking
the old role.

## Rollback

Keep `flush` paused (or the service stopped, which halts crediting on every route) and deploy a
corrected new version/key. A compromised key is never
re-granted. During routine rotation only, a non-compromised old key may be retained briefly until
the new operator has completed verification; any re-grant still requires explicit Security and
Finance approval through the Safe.
