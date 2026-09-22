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

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
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
token/RPC/config issue, wait until `retry_after`, and restart `topup` only if the loop is stopped.
Never mark a flush confirmed or edit exclusions in SQL.

## Verification

A new flush row confirms with finalized `Flushed` logs, unaffected addresses are processed, isolated
address balance is accounted for, and treasury delta equals stored events. Resume flush.

## Rollback

Re-pause flush. If a config upgrade caused the failure, redeploy the retained prior compose hash;
reverted rows remain immutable evidence.
