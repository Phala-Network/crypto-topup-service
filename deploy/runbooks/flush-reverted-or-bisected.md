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

Pause the `flush` scope on the route. Since C7b (#70) this is a real stop for new sends: the planner
skips paused addresses, and the sender re-checks the pause under the operator lock, leaves a paused
plan `planned`, and writes one `flush.send_paused` audit row for it. A transaction already broadcast
(`sent`) is not recalled; the flusher keeps confirming or replacing it.

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-before-pause
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

After at least one flusher maintenance interval, verify that no new row became `sent` after the
pause and read the pause time, the waiting plans, and their `flush.send_paused` audit rows.
`flushes` has no creation timestamp, so the id snapshot is the comparison and the pause audit row
supplies the time:

```sh
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' ORDER BY id; COMMIT;" > /tmp/sent-after-pause
comm -13 /tmp/sent-before-pause /tmp/sent-after-pause > /tmp/new-sent-after-pause
test ! -s /tmp/new-sent-after-pause
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT max(created_at) AS paused_at FROM audit
WHERE action='pause' AND subject='route:' || :'route';
SELECT id,operator,nonce::text,status FROM flushes
WHERE chain_id=:chain_id AND status='planned' ORDER BY nonce;
SELECT subject AS flush_id,reason,created_at FROM audit
WHERE action='flush.send_paused' AND created_at >= (
  SELECT max(created_at) FROM audit WHERE action='pause' AND subject='route:' || :'route'
) ORDER BY created_at;
COMMIT;
SQL
```

A plan that existed when the pause landed appears with one `flush.send_paused` row; the sent-id
difference must be empty.

**Caveat ([#71](https://github.com/Phala-Network/crypto-topup-service/issues/71)):** the sender always
takes the lowest-nonce plan for the chain and operator. A paused plan at the lowest nonce therefore
stalls every later flush on that chain, including other routes and accounts, until the pause is
lifted; `main` has no supported command to void such a plan. For a targeted stop of one account or
product, pause before a plan containing it exists; if a paused plan already heads the queue, keep
the stall short or accept it chain-wide.

For an emergency chain-wide stop, including a suspected operator compromise, the Finance Safe
revoking `OPERATOR_ROLE` remains the hard stop because it does not depend on the service:

**HUMAN-ONLY, Finance Safe, when a chain-level stop is required:**

```sh
export OPERATOR_ROLE="$(cast keccak OPERATOR_ROLE)"
cast calldata 'revokeRole(bytes32,address)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_A_URL"
cast call "$FACTORY" 'hasRole(bytes32,address)(bool)' "$OPERATOR_ROLE" "$OPERATOR_ADDRESS" --rpc-url "$RPC_PROVIDER_B_URL"
```

Collect the flush evidence:

```sh
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
token/RPC/config issue and wait until `retry_after`, then resume the `flush` scope with the same
signed request against `/v1/admin/routes/$ROUTE/resume`. If the role was revoked, **HUMAN-ONLY**
re-grant only the verified, non-compromised operator through the Safe before resuming. Never mark a flush confirmed or edit
exclusions in SQL.

## Verification

A new flush row confirms with finalized `Flushed` logs, unaffected addresses are processed, the
isolated address balance is accounted for, and treasury delta equals stored events. After resume,
no plan remains `planned` behind a `flush.send_paused` row and later nonces send again.

## Rollback

Re-pause the `flush` scope and, if required, revoke `OPERATOR_ROLE`. If a config upgrade caused the
failure, redeploy the retained prior compose hash; reverted rows remain immutable evidence.
