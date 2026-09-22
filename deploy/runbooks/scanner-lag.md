# Scanner lag

## Trigger

Trigger on `TopupScannerLag` (PR #56: no successful scan for 120 s or `topup_scanner_lag_blocks`
above zero for five minutes) or `TopupLoopStopped{loop="scanner"}`. If the chain has a
`reconciliation_blocks` row with `scope='chain'`, the scanner is paused on purpose: follow
[Chain frozen](chain-frozen.md) instead.

## Impact and blast radius

New finalized transfers are not detected, so customers wait. Existing rows can continue through
the pump. Blast radius is one chain and every route on it.

## First 5 minutes

```sh
cast block finalized --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {number,hash}'
cast block finalized --json --rpc-url "$RPC_PROVIDER_B_URL" | jq '(.data // .) | {number,hash}'
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT chain_id,scanned_block FROM cursors ORDER BY chain_id; SELECT state,count(*) FROM deposits GROUP BY state ORDER BY state; COMMIT;"
printf '%s' '{"scopes":["quotes","addresses"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

Pause address/quote issuance when lag is material; existing addresses remain valid and monitored.

## Decision tree

- Both providers healthy and cursor static: inspect/restart the service loop.
- One provider unhealthy: follow provider disagreement/config replacement.
- Cursor advances but lag grows: provider rate limit or scan workload is insufficient.

## Remediation

Restart only the service container after preserving logs; scanner resumes from the committed cursor:

```sh
docker compose -f deploy/docker-compose.staging.yml logs --no-color --tail=300 topup
docker compose -f deploy/docker-compose.staging.yml restart topup
```

Provider or batching changes require a reviewed attested config upgrade; never advance the cursor in
SQL.

## Verification

Cursor catches the common finalized height, backfilled deposits appear once, duplicate logs do not
create duplicate rows, and both providers agree. Resume addresses and quotes.

## Rollback

If restart worsens lag, re-pause issuance and redeploy the retained prior compose hash. The cursor
must remain at its last committed value.
