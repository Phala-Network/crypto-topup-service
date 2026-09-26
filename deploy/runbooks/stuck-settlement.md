# Stuck settlement

**Trigger:** `TopupDepositStateAgeExceeded` with `state:cleared` (older than the route's
`alerts.stuck_after_s`), a product that keeps answering `processing` or `409`, or a
`topup-pump-<n>` monitor missing its check-ins (the pump that drives every step has stopped).

**Impact:** the product has not given a final credited or rejected answer. One deposit or one
product; funds stay attributable and flushing is independent.

## First steps

1. For a stopped pump: the whole pipeline waits. **HUMAN-ONLY:** restart the CVM
   (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`); pumps resume from the database.
2. Otherwise take the `deposit_id` from the Sentry event and read the settlement attempts in its
   timeline (support lookup): the latest `evidence` holds the product's answer or the error.
3. Only for a systemic product fault, pause settlement on the route:
   `admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["settlement"]}'`.

## Decide

- Product `GET` answers accepted or rejected: the service adopts it on the next attempt.
- Product answers `processing`: keep waiting and escalate on the product side.
- Product does not know the key and no `POST` was durably sent: the service resends by itself.
- The product answered `422`: [422 payload mismatch](payload-mismatch-422.md).

## Fix

Nudge the deposit, which sets its next attempt to now and writes an audit record; never change the
deposit any other way:

```sh
admin POST "/v1/admin/deposits/$DEPOSIT_ID/nudge"
```

## Done when

The deposit is `credited` or `rejected`, the product ledger holds exactly one mutation for it, and
any pause is resumed. Never resend a changed payload.
