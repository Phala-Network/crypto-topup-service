# Outbox backlog

**Trigger:** the `topup-outbox-<n>` monitor missing its check-ins, `outbox delivery poll failed`
issues, or a product reporting that webhooks stopped.

**Impact:** deposit states stay authoritative; only notifications are late. Deliveries retry with
backoff by themselves. One product's receiver or every product.

## First steps

1. Read the error of `outbox delivery poll failed` in Sentry: a database error stops every
   delivery; a receiver's failures do not raise an issue.
2. With the product, check its webhook endpoint, TLS, and signature verification. The product can
   catch up at any time by fetching state (deposits, rate locks), which receivers must act on
   anyway.
3. For a missing event, the product's signed support lookup lists each deposit's `events`: `id`
   (the `webhook-id`), `event_type`, and `delivered_at` (`null` while undelivered). Delivery
   attempts are not observable in production.

## Decide

- Receiver down or answering `5xx`: fix the receiver; deliveries resume by themselves.
- An event is still missing after the fix, or the receiver lost one it accepted: replay it. A
  delivered event is sent again and a pending one becomes due now, with the same id and payload;
  a repeat while it is due changes nothing:

  ```sh
  admin POST "/v1/admin/outbox/$EVENT_ID/replay" '{"reason":"INC-123: receiver lost the event"}'
  ```

- Receiver rejects signatures (`4xx`): coordinate its settlement-key pinning.
- Monitor silent with no error: the delivery worker stopped. **HUMAN-ONLY:** restart the CVM
  (`npx --yes phala@1.1.22 cvms restart "$TOPUP_CVM_ID"`).

## Done when

`topup-outbox-<n>` checks in again and the product receives new events; receivers deduplicate by
webhook id.
