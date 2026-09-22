# Operator key compromise

## Trigger

Trigger on an operator transaction not linked to a `flushes` row, an unexpected pending nonce,
`MissingConsumedReceipt`, or evidence that operator key material was exposed. PR #56 has not
defined an exact alert name on `main`.

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

Both calls must return `false`. If Safe execution is delayed, stop the service container as an
additional local control, but do not treat that as a substitute for revocation:

```sh
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='planned' ORDER BY id; COMMIT;" > /tmp/planned-before-stop
test -s /tmp/planned-before-stop
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-before-stop
docker compose -f deploy/docker-compose.staging.yml stop topup
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-after-stop
comm -13 /tmp/sent-before-stop /tmp/sent-after-stop > /tmp/new-sent-after-stop
test ! -s /tmp/new-sent-after-stop
```

The `flush` pause scope is currently bookkeeping only: the flusher does not honor it. Track this
behavior gap in [#61](https://github.com/Phala-Network/crypto-topup-service/issues/61).

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

**HUMAN-ONLY:** provision a new, attested operator and grant its role through the Safe. Current
`main` cannot derive/select `operator/v2`; [#60](https://github.com/Phala-Network/crypto-topup-service/issues/60)
must land before the service can use the replacement key. Never grant an arbitrary EOA as a
workaround.

```sh
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

After a release supporting the new key is attested and started, C7 automatically re-binds stale
`planned` rows to the new operator and its nonce sequence. Verify the rows and append-only audit:

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
`flush.plan_operator_rebound` audit record per re-bound plan before restarting normal operation.

### Verification

Require the old role to remain false on both providers, the new role to be true, no unexpected sent
row after the stop, no stale old-operator plans, and expected rebound audits.

## Routine rotation

For a scheduled rotation with no compromise, use the architecture's short overlap procedure:

1. **HUMAN-ONLY:** grant the attested new operator through the Finance Safe.
2. Deploy the attested service version that derives the new operator; this depends on #60.
3. Verify both roles, the new operator nonce, one successful flush, and any C7 plan-rebind audits.
4. **HUMAN-ONLY:** revoke the old role through the Finance Safe after the new service is healthy.

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
no unexpected sent row after the stop, no stale old-operator plans, and expected rebound audits.
For routine rotation, additionally require a confirmed flush from the new operator before revoking
the old role.

## Rollback

Keep the service stopped and deploy a corrected new version/key. A compromised key is never
re-granted. During routine rotation only, a non-compromised old key may be retained briefly until
the new operator has completed verification; any re-grant still requires explicit Security and
Finance approval through the Safe.
