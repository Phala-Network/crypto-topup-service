# Stuck settlement

## Trigger

Trigger on `TopupDepositStateAgeExceeded` (`state:cleared`) (a deposit exceeded the route's
`alerts.stuck_after_s`), a settlement that remains `sent`, or a product that repeatedly answers
`processing`/`409`.

## Impact and blast radius

The product has not established a final credited/rejected fact. One deposit or one product can be
affected; funds remain attributable and flushing is independent unless reconciliation blocks it.

## First 5 minutes

```sh
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 <<< "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.state,d.attempt,d.next_attempt_at,d.updated_at,s.key,s.status,s.sent_at,s.destination_tx_id,s.resend_forbidden,s.receipt FROM deposits d LEFT JOIN settlements s ON s.deposit_id=d.id WHERE d.state='cleared' ORDER BY d.updated_at LIMIT 100; COMMIT;"
printf '%s' '{"scopes":["settlement"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
```

Pause only for a systemic product fault; for one deposit, allow GET-first recovery to continue.

## Decision tree

- Product GET says accepted/rejected: service should adopt it; verify the next pump attempt.
- Product GET says processing: keep waiting and escalate product-side age.
- Product says unknown and no POST was durably sent: automatic resend is allowed.
- `resend_forbidden=true`: follow the 422 runbook.

## Remediation

Nudge the deposit to set `next_attempt_at=now()` and append audit evidence; do not update the row
directly:

```sh
: > /tmp/empty
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/deposits/$DEPOSIT_ID/nudge" /tmp/empty "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/empty "$BASE_URL/v1/admin/deposits/$DEPOSIT_ID/nudge"
```

## Verification

The settlement becomes `accepted` or `rejected`, exactly one product ledger mutation exists, and
the deposit advances accordingly. Resume settlement if it was paused.

## Rollback

Re-pause settlement if product responses regress. Never replay a POST with a changed payload or
delete the settlement row.
