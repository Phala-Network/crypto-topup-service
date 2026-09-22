# Flush reverted or bisected

## Trigger

Trigger on a `flushes.status='reverted'`, `FlushAlert::Reverted`, `IsolatedAddress`,
`PlanningExcluded`, or repeated singleton estimation failure, or on
`TopupLoopStopped{loop="flusher"}` from PR #56. PR #56 has no dedicated reverted-flush alert.

## Impact and blast radius

The reverted batch transfers nothing because factory batches are atomic. A persistently failing
address is excluded while other addresses continue after bisection. Treasury exposure grows for the
affected token/address set.

## First 5 minutes

**Gap:** the `flush` pause scope is recorded by the API but the flusher does not honor it until
[#61](https://github.com/Phala-Network/crypto-topup-service/issues/61) (C7b) lands; it is not a
stop. The verified manual stop is to stop the service container and, when the operator itself must
be prevented from sending, have the Finance Safe revoke `OPERATOR_ROLE`. The signed pause below only
records operator intent for customers and the audit trail.

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-before-stop
docker compose -f deploy/docker-compose.staging.yml stop topup
```

**HUMAN-ONLY, Finance Safe, when chain-level stop is required:**

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
```

Verify that no new row became `sent` after the stop:

```sh
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-after-stop
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
cast receipt "$FLUSH_TX_HASH" --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {status,blockNumber,transactionHash,logs}'
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
isolated address balance is accounted for, and treasury delta equals stored events. Resume the API
pause record once the service is running again; it does not control the flusher until #61 lands.

## Rollback

Stop the service again and, if required, revoke `OPERATOR_ROLE`. If a config upgrade caused the
failure, redeploy the retained prior compose hash; reverted rows remain immutable evidence.
