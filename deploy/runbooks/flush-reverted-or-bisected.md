# Flush reverted or bisected

## Trigger

Trigger on a `flushes.status='reverted'`, `FlushAlert::Reverted`, `IsolatedAddress`,
`PlanningExcluded`, or repeated singleton estimation failure. PR #56 has no exact alert name on
`main`.

## Impact and blast radius

The reverted batch transfers nothing because factory batches are atomic. A persistently failing
address is excluded while other addresses continue after bisection. Treasury exposure grows for the
affected token/address set.

## First 5 minutes

The `flush` pause API is not an effective stop until [#61](https://github.com/Phala-Network/crypto-topup-service/issues/61)
lands. Record the intent, then stop the local service container and/or have the Finance Safe revoke
`OPERATOR_ROLE`. Revocation is required if the operator itself must be prevented from sending.

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='planned' ORDER BY id; COMMIT;" > /tmp/planned-before-stop
test -s /tmp/planned-before-stop
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-before-stop
docker compose -f deploy/docker-compose.staging.yml stop topup
```

**HUMAN-ONLY, Finance Safe, when chain-level stop is required:**

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

Verify that a non-empty planned set existed and no new row became `sent` after the stop:

```sh
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" -c "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-after-stop
comm -13 /tmp/sent-before-stop /tmp/sent-after-stop > /tmp/new-sent-after-stop
test ! -s /tmp/new-sent-after-stop
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT id,operator,nonce::text,status,tx_hash,block_number,receipt FROM flushes
WHERE chain_id=:chain_id ORDER BY nonce DESC LIMIT 30;
SELECT chain_id,token,address_id,reason,retry_after,failures
FROM flush_exclusions ORDER BY updated_at DESC LIMIT 50;
COMMIT;
SQL
cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '{status,blockNumber,transactionHash,logs}'
```

## Decision tree

- One revert, no singleton failure: runtime retries a fresh nonce, then bisects on persistence.
- Isolated address: verify token behavior and forwarder balance; keep only that address excluded.
- Missing consumed receipt: follow operator compromise/provider investigation.
- Fee cap reached: review gas policy, not the batch contents.

## Remediation

The flusher owns fresh-nonce retry and bisection; there is no manual flush CLI. Fix the underlying
token/RPC/config issue and wait until `retry_after`. If only the service was stopped, start it after
verification. If the role was revoked, **HUMAN-ONLY** re-grant only the verified, non-compromised
operator through the Safe before starting the service. Never mark a flush confirmed or edit
exclusions in SQL.

## Verification

A new flush row confirms with finalized `Flushed` logs, unaffected addresses are processed, the
isolated address balance is accounted for, and treasury delta equals stored events. The API pause
record may be resumed, but it does not control the flusher until #61 lands.

## Rollback

Stop the service again and, if required, revoke `OPERATOR_ROLE`. If a config upgrade caused the
failure, redeploy the retained prior compose hash; reverted rows remain immutable evidence.
