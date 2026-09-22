# Provider disagreement

## Trigger

Trigger when the two configured providers return different finalized block hashes, log evidence,
or sanctions results. The current symptom is deposits remaining `detected` with disagreement
evidence; PR #56 alert names are not on `main`.

## Impact and blast radius

Affected deposits cannot safely confirm. No credit should occur, but customer deposits on the
route wait for agreement. Other chains and routes are unaffected.

## First 5 minutes

```sh
printf '%s' '{"scopes":["settlement"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
cast block finalized --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '{number,hash}'
cast block finalized --json --rpc-url "$RPC_PROVIDER_B_URL" | jq '{number,hash}'
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT d.id,d.tx_hash,d.log_index,d.block_number,d.block_hash,d.state,d.updated_at,t.evidence
FROM deposits d
LEFT JOIN LATERAL (
  SELECT evidence FROM transitions WHERE deposit_id=d.id ORDER BY created_at DESC LIMIT 1
) t ON true
WHERE d.chain_id=:chain_id AND d.state='detected'
ORDER BY d.updated_at LIMIT 50;
COMMIT;
SQL
```

## Decision tree

- Same finalized height, different hash: treat as critical; keep settlement paused.
- One provider behind but internally consistent: wait within provider SLA, then escalate/replace.
- Same chain evidence but sanctions answers differ: keep the deposit waiting and page Compliance.

## Remediation

**HUMAN-ONLY:** open provider incidents with the exact block/log evidence. Provider membership is
attested configuration, not runtime state. Replacing a provider requires a new route/config version,
`topup route validate`, immutable image digests, a new compose hash, Safe allow-list approval, and
the D2 upgrade flow.

## Verification

Both `cast block finalized` calls must agree, affected logs must match, and deposit attempts must
advance without manual database writes. Resume settlement with the same signed curl pattern against
`/v1/admin/routes/$ROUTE/resume`.

## Rollback

If the replacement provider disagrees, re-deploy the retained prior compose hash only if its two
providers are healthy; otherwise keep settlement paused and restore the last known-good provider
pair in a new attested version.
