# Product key compromise

**Trigger:** the product reports that its request-signing seed was exposed, or product-signed
requests it did not make (accounts, addresses, rate locks, pauses, refund requests). The service
raises no alert for this: only the product can tell its own requests from others.

**Impact:** whoever holds the seed can call the product API as the product: register accounts,
issue addresses and rate locks (within the exposure caps and rate limits), cancel locks, pause
accounts, read deposits, and request refunds. It cannot move funds or credit: forwarders pay only
the treasury, credits exist only as `deposit.credited` events the service signs and delivers to
the registered webhook URL, and every refund waits for Finance, which confirms a refund of a
credited deposit with the product before approving it.

## Replace the key

Replacement is a hard cut (architecture §15, Rotation): the service verifies a product against one
stored key under the key id its routes name, so the old key fails from the moment the new one is
stored, and the product's requests fail until it signs with the new seed.

1. The product generates a new key under the same key id, `{slug}/v1`
   (`topup-sdk keygen --keyid <slug>/v1`), and sends the printed `public_key`; confirm it with
   the product's owner over a second channel. If the product cannot do this promptly, pause
   `quotes`, `addresses`, and `refunds` on each of its routes meanwhile
   (`admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["quotes","addresses","refunds"]}'`);
   deposits keep being credited.
2. Store it, with the product's current webhook URL (or its new one):

```sh
admin PUT /v1/admin/products/phala-cloud \
  '{"public_key":"<new base64>","webhook_url":"https://product.example/topup/webhooks","reason":"product key compromise: <incident>"}'
```

   The answer is `200` with the new `public_key`; the `audit` row `product.update` records the
   reason and the replaced key.
3. The product switches its signer to the new seed. Resume any scope paused in step 1.
4. Review what the old key could have done: confirm every refund request since the exposure with
   the product before Finance approves it (daily report `refunds_by_status`), and let the product
   check its accounts' pause scopes and open rate locks.

## Done when

The product's requests signed with the new seed succeed, a request signed with the old seed
answers `401`, and every refund request since the exposure is confirmed or declined. A planned
rotation without compromise is the same `PUT`, timed with the product's signer switch
([integration guide](../../docs/integration.md#34-rotate-the-product-key)).
