# Outbox backlog

**Trigger:** the `topup-outbox-test` or `topup-outbox-live` monitor missing its check-ins, `outbox delivery poll failed`
issues, a non-zero `credited_undelivered` in the daily report (`GET /v1/admin/report/daily`), or
a merchant reporting that webhooks or credits stopped.

**Impact:** `deposit.credited` is how the merchant learns it owes a credit, so an undelivered one
means a user not credited yet. Deposit states stay authoritative and deliveries retry with backoff
(capped at 1 h) until delivered, by themselves: a failing endpoint is never disabled (owner
decision, design §11), and is probed about once an hour, one delivery at a time. Only the
receiver's `410 Gone` or the merchant disables an endpoint. One merchant's receiver or every
merchant. There is no email channel: the recorded contact is how the operator reaches a merchant
whose endpoint keeps failing.

## First steps

1. Read the error of `outbox delivery poll failed` in Sentry: a database error stops every
   delivery; a receiver's failures do not raise an issue.
2. With the merchant, from its recorded contact, check its webhook endpoints (`GET
   /v1/webhook_endpoints`: `status`, `disabled_reason`), TLS, and signature verification. Report
   `credited_undelivered` and `credited_undelivered_max_age_seconds` per route. The merchant
   credits only from signed events, so it cannot catch up by fetching state alone.
3. For a missing event, the admin deposit view lists each deposit's `events`: `id` (the
   `webhook-id`), `event_type`, and `delivered_at` (`null` while undelivered); the merchant sees
   the same event with `pending_webhooks` in `GET /v1/events`. Delivery attempts are not
   observable in production.

## Decide

- Receiver down or answering `5xx`, however long: tell the merchant through its recorded contact;
  once it fixes the receiver, the next probe succeeds and the backlog drains by itself.
- Endpoint disabled (by the merchant, or after it answered `410 Gone`), or an event lost by a
  receiver that accepted it: the merchant re-enables the endpoint (`POST
  /v1/webhook_endpoints/{id} {"disabled": false}`) and resends each missed event with its own key
  (`POST /v1/events/{id}/resend {"webhook_endpoint"}`, docs/integration.md §5.11), same id and
  payload. The operator has no replay: it does not act on a merchant's webhooks (design §2).
- A URL whose host resolves to a private, CGNAT, or metadata address is refused by the egress proxy
  ([webhook egress](../README.md#webhook-egress)) and fails like an unreachable one: the merchant
  must use a public address.

- Receiver rejects signatures (`4xx`): coordinate its settlement-key pinning.
- Monitor silent with no error: the delivery worker stopped. **HUMAN-ONLY:** restart the CVM
  (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`).

## Done when

`topup-outbox-<mode>` checks in again and the merchant receives new events; receivers deduplicate
by webhook id.
