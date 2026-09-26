# Provider disagreement

## Trigger

Trigger when the two configured providers return different finalized block hashes or log evidence,
when `TopupDepositStateAgeExceeded` (`state:detected`) fires, or when sanctions screening is
unavailable or reports a hit (screening runs in `confirmed → cleared`, so an unavailable screen
shows as `TopupDepositStateAgeExceeded` (`state:confirmed`)).

## Impact and blast radius

Chain-evidence disagreement keeps affected deposits retrying in `detected`. Sanctions screening
has different precedence: any provider returning `Sanctioned` rejects the deposit immediately;
`Unavailable` retries only when neither provider reports `Sanctioned`.

## First 5 minutes

Compare both providers and read the latest step evidence for waiting deposits. Rows in `detected`
carry chain evidence; rows in `confirmed` carry the per-provider sanctions answers:

```sh
cast block finalized --json --rpc-url "$RPC_PROVIDER_A_URL" | jq '(.data // .) | {number,hash}'
cast block finalized --json --rpc-url "$RPC_PROVIDER_B_URL" | jq '(.data // .) | {number,hash}'
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 --set=chain_id="$CHAIN_ID" <<'SQL'
BEGIN TRANSACTION READ ONLY;
SELECT d.id,d.tx_hash,d.log_index,d.block_number,d.block_hash,d.state,d.attempt,d.updated_at,
       t.evidence
FROM deposits d
LEFT JOIN LATERAL (
  SELECT evidence FROM transitions WHERE deposit_id=d.id ORDER BY created_at DESC LIMIT 1
) t ON true
WHERE d.chain_id=:chain_id AND d.state IN ('detected','confirmed')
ORDER BY d.updated_at LIMIT 50;
SELECT id,state,reason,from_address,updated_at FROM deposits
WHERE chain_id=:chain_id AND state='rejected' AND reason='sanctioned'
ORDER BY updated_at DESC LIMIT 20;
COMMIT;
SQL
```

For chain-evidence disagreement only, pause settlement on the route while providers are
investigated. Do not pause for a sanctions hit; the screen step rejects it regardless:

```sh
printf '%s' '{"scopes":["settlement"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

## Decision tree

- Same finalized height, different hash or log: the step retries and the age alert fires; keep
  settlement paused. Never pick a provider's answer by hand.
- Latest evidence `error = log_absent_at_finality`: both providers are final past the row and
  neither has the log the scanner recorded as final. This is a finality violation or a scanner
  provider fault, not a disagreement; keep settlement paused, open incidents with both providers,
  and escalate. Never delete or reject the row by hand.
- One chain provider behind but internally consistent: retry within provider SLA, then
  escalate/replace.
- Any sanctions provider returns `Sanctioned`: the deposit is `rejected(sanctioned)` immediately,
  before pause state is considered and whatever the other provider says. There is no waiting state
  for a sanctions hit; go to the compliance path below.
- At least one sanctions provider returns `Unavailable` and neither reports `Sanctioned`: retry the
  screen step and alert after the route's age threshold.
- Both sanctions providers return `Clear`: continue normal screening and apply pause state only
  afterward.

## Remediation

**HUMAN-ONLY:** open provider incidents with the exact block/log evidence. Provider membership is
attested configuration, not runtime state. Replacing a provider requires a new route/config version,
`topup route validate`, immutable image digests, a new compose hash, and Deploy in mode `upgrade`
([deploy/README.md, "Deploy"](../README.md#deploy)).

For a sanctions rejection, page Compliance and follow
[Rejected funds at treasury](rejected-funds-at-treasury.md). The rejected funds still flush to the
treasury, but Finance must not initiate an automatic refund or other transfer until Compliance has
recorded the disposition.

## Verification

For chain disagreement, both `cast block finalized` calls must agree, affected logs must match, and
deposit attempts must advance without manual database writes. For sanctions, verify the recorded
provider answers and either `rejected(sanctioned)` or a retry transition exactly matches the truth
table above. Resume settlement with the same signed curl pattern against
`/v1/admin/routes/$ROUTE/resume` only after chain evidence is healthy.

## Rollback

If the replacement provider disagrees, re-deploy the retained prior compose hash only if its two
providers are healthy; otherwise keep settlement paused and restore the last known-good provider
pair in a new attested version.
