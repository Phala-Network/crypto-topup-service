# Flush reverted or bisected

## Trigger

Trigger on a `flushes.status='reverted'`, `FlushAlert::Reverted`, `IsolatedAddress`,
`PlanningExcluded`, or repeated singleton estimation failure, or on an `error` or missed check-in of
the Sentry Crons monitor `topup-flush-<route>`.

## Impact and blast radius

The reverted batch transfers nothing because factory batches are atomic. A persistently failing
address is excluded while other addresses continue after bisection. Treasury exposure grows for the
affected token/address set.

## First 5 minutes

Pause the `flush` scope on the route. Since C7b (#70) this is a real stop for new sends: the planner
skips paused addresses, and the sender re-checks the pause under the operator lock. Since #71 an
unsigned plan covered by a route, product, or account `flush` pause is voided when the sender
reaches it: the `planned` row is removed, one `flush.send_paused` audit row names the flush id and
the paused level, later plans on the chain move down onto its nonce and keep sending, and the
addresses are planned again after resume. A transaction already broadcast (`sent`) is not recalled;
the flusher keeps confirming or replacing it. Take the `sent` snapshot only after the pause request
returns HTTP 200. The sender re-checks the pause under the operator lock, including a route's first
pause, so no new send for the route should pass afterwards, while a legitimate send just before the
pause would otherwise look like a failure. Other routes on the same chain keep sending, so the
snapshot is filtered to this route.

```sh
printf '%s' '{"scopes":["flush"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' AND receipt->'binding'->>'route'=:'route' ORDER BY id; COMMIT;" > /tmp/sent-before-pause
```

After at least one flusher maintenance interval, verify that no new row became `sent` after the
pause and read the pause time, the remaining plans, and the `flush.send_paused` audit rows.
`flushes` has no creation timestamp, so the id snapshot is the comparison and the pause audit row
supplies the time:

```sh
sleep 15
psql "$DATABASE_URL" -XAtq -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" --set=route="$ROUTE" <<< "BEGIN TRANSACTION READ ONLY; SELECT id FROM flushes WHERE chain_id=:chain_id AND status='sent' AND receipt->'binding'->>'route'=:'route' ORDER BY id; COMMIT;" > /tmp/sent-after-pause
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

The sent-id difference must be empty. Expect one `flush.send_paused` row per plan of this route that
the sender reached after the pause; plans of the route still `planned` are voided when the sender
reaches them. An empty audit result is correct when the route had no plan queued.

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
- Pause during bisection: a paused revert replan is voided like any other plan, so its
  `failure_attempt` and `parent_flush_id` are discarded and bisection restarts from a fresh plan
  after resume. Isolation then depends on gas estimation excluding the failing address (`flush_exclusions`,
  `PlanningExcluded`) or on the fresh plan reverting again; expect `IsolatedAddress` later than
  without the pause, and keep the pause short while a bisection is in progress.

## Remediation

The flusher owns fresh-nonce retry and bisection; there is no manual flush CLI. Fix the underlying
token/RPC/config issue and wait until `retry_after`, then resume the `flush` scope with the same
signed request against `/v1/admin/routes/$ROUTE/resume`. If the role was revoked, **HUMAN-ONLY**
re-grant only the verified, non-compromised operator through the Safe before resuming. Never mark a flush confirmed or edit
exclusions in SQL.

## Verification

A new flush row confirms with finalized `Flushed` logs, unaffected addresses are processed, the
isolated address balance is accounted for, and treasury delta equals stored events. After resume,
the addresses of any voided plan are planned again and their new flush sends.

## Rollback

Re-pause the `flush` scope and, if required, revoke `OPERATOR_ROLE`. If a config upgrade caused the
failure, redeploy the retained prior compose hash; reverted rows remain immutable evidence.
