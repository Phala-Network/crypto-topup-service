# 422 payload mismatch

## Trigger

Trigger when the settlement endpoint returns HTTP `422` for an existing idempotency key. On
`main`, the durable symptom is `settlements.resend_forbidden=true` with receipt status
`payload_mismatch`.

## Impact and blast radius

The service and product disagree about immutable settlement payload bytes. The affected deposit
must not be resent or re-priced. Repeated mismatches may indicate restore/config corruption and
can affect one product route.

## First 5 minutes

```sh
printf '%s' '{"scopes":["settlement"]}' > /tmp/pause.json
mapfile -t headers < <(deploy/runbooks/sign-admin-request.sh POST "$BASE_URL/v1/admin/routes/$ROUTE/pause" /tmp/pause.json "$ADMIN_KEY_FILE" "$ADMIN_KEY_ID")
curl --fail-with-body -sS -X POST -H 'content-type: application/json' -H "${headers[0]}" -H "${headers[1]}" -H "${headers[2]}" --data-binary @/tmp/pause.json "$BASE_URL/v1/admin/routes/$ROUTE/pause"
psql "$DATABASE_URL" -v ON_ERROR_STOP=1 -c "BEGIN TRANSACTION READ ONLY; SELECT d.id,d.tx_hash,d.log_index,d.route,d.route_version,d.valuation_at,d.price_scaled::text,d.credit_minor::text,s.key,s.payload,s.status,s.receipt,s.resend_forbidden FROM deposits d JOIN settlements s ON s.deposit_id=d.id WHERE s.resend_forbidden ORDER BY d.updated_at DESC LIMIT 50; COMMIT;"
```

**HUMAN-ONLY:** preserve the product's original stored payload and response under the incident ID.

## Decision tree

- Product original payload equals local payload: product idempotency implementation is faulty.
- Product original payload differs, but chain evidence matches: investigate restore/config version.
- Chain evidence differs: escalate to critical integrity incident; keep settlement paused.

## Remediation

The product's accepted/rejected fact and original payload are authoritative. Allow normal GET-first
adoption after the product team confirms its record. There is no supported command to clear
`resend_forbidden`; do not modify it in SQL and do not use outbox replay for settlement requests.

## Verification

Verify the deposit adopts the product fact without a new POST, the stored valuation matches the
product's original payload, and no duplicate destination transaction exists. Resume settlement
only after all mismatches are classified.

## Rollback

Re-pause settlement. Roll back only by deploying the retained attested compose/config version;
never roll back product ledger facts or delete idempotency records.
