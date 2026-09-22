# Operator key compromise

## Trigger

Trigger on an operator transaction not linked to a `flushes` row, an unexpected pending nonce,
`MissingConsumedReceipt`, or evidence that `operator/v1` material was exposed. PR #56 has not
defined an exact alert name on `main`.

## Impact and blast radius

The operator can spend only its gas and call `ForwarderFactory.flush`; funds can move only to the
immutable treasury. Blast radius is unnecessary gas spend, forced flush timing, and all routes on
factories where the address has `OPERATOR_ROLE`.

## First 5 minutes

Pause flush for the affected route, preserve transaction evidence, and page Security and Finance:

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST \
  "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' \
  -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
  --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,operator,nonce::text,status,tx_hash,receipt FROM flushes
WHERE chain_id=:chain_id ORDER BY nonce DESC LIMIT 20;
COMMIT;
SQL
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast nonce "$OPERATOR_ADDRESS" --block pending --rpc-url "$RPC_PROVIDER_A_URL"
cast balance "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

## Decision tree

- Unknown transaction or confirmed key exposure: keep `flush` paused and rotate.
- Only a known replacement transaction: verify its hash history in `receipt`; continue monitoring.
- Provider A alone reports the transaction: follow provider disagreement before rotating.

## Remediation

**HUMAN-ONLY, Finance Safe:** obtain the attested `operator/v2` address, grant it, verify on both
providers, then revoke `operator/v1` only after the new service is healthy:

```sh
cast calldata 'grantRole(bytes32,address)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$NEW_OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
```

`main` cannot derive/select `operator/v2`; keep flush paused and record this command gap until a
reviewed release adds it. Do not grant another arbitrary EOA as a workaround.

## Verification

Confirm the old role is false, the new role is true on both providers, the new pending nonce is
zero or expected, and no unexpected flush row appears. Resume only flush:

```sh
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST \
  "$BASE_URL/v1/admin/routes/$ROUTE/resume" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' \
  -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" \
  --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/resume"
```

## Rollback

Re-pause flush. If the new key is faulty but not compromised, the Safe may re-grant the old role
only after Security explicitly approves; otherwise deploy a corrected versioned key and repeat.
