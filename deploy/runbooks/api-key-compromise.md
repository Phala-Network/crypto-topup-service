# API key compromise and key recovery

**Trigger:** a merchant reports that a secret key (`ppay_sk_…`) or a restricted key (`ppay_rk_…`)
was exposed, that it lost every
key of a mode, or that requests it did not make appear in its `api_key.*` events; or GitHub secret
scanning reports a Phala Pay key. The service raises no alert for this: only the merchant can
tell its own requests from others.

**Impact:** whoever holds a secret key acts as the merchant in that key's mode: it can create
quotes (within the caps and rate limits), cancel them, read deposits, create and cancel refunds,
and manage the mode's keys. It cannot move funds: forwarders pay only the treasury, credits exist
only as `deposit.credited` events the service signs and delivers to the account's webhook
endpoints, and a refund succeeds only once the merchant pays it from its treasury.
It can also prove a new live treasury, but that change waits 48 hours, is announced at once as
`treasury.created` to every enabled endpoint of the mode whatever its subscriptions, and
the merchant cancels it with `POST /v1/treasuries/{id}/cancel` ([Treasury change](treasury-change.md)).
It cannot silence the account's notices by rolling the webhook key: a live roll keeps the pinned
key signing for 48 hours and signs its own `account.updated` with it. A restricted key, which
merchants run production with, holds only its granted permissions and can do none of this: no key,
treasury, webhook endpoint, webhook key, or account setting changes.

## The merchant rolls the key

A merchant that still holds a working key replaces a leaked one itself (design D7), without the
operator: `POST /v1/api_keys/{id}/roll {"expires_in": 0}` returns a new key and revokes the old
one at once, and `DELETE /v1/api_keys/{id}` revokes any other key. The service refuses to revoke
the mode's last key that is neither revoked nor expiring, so the account always keeps one.

## Recovery by the operator

When the merchant lost every key of a mode, or suspects a leak it cannot win by rolling (the
attacker rolls too):

1. Verify the request with the account's recorded contact over a second channel (the `contact`
   of `POST /v1/admin/accounts`). Never act on the request's own channel alone.
2. Issue a recovery key, revoking the mode's keys first when the merchant asks:

```sh
admin POST "/v1/admin/accounts/$ACCOUNT/api_keys" \
  '{"livemode":false,"revoke_existing":true,"reason":"key recovery: <how the contact confirmed>"}'
```

   The answer is `200` with the key's `secret`, shown only once; the `audit` rows
   `api_key.revoked` and `api_key.created` and the account's `api_key.*` events carry actor
   `admin`. A live key needs the account enabled for live mode (`403 testmode_charges_only`
   otherwise).
3. Send the key to the contact through an encrypted channel. The merchant rolls it on receipt, so
   no one at Phala holds a working key.
4. Review what the old key could have done: the merchant reviews every refund created since the
   exposure (`GET /v1/refunds`; the daily report's `refunds_by_status` counts them per route) and
   cancels those it did not make (`POST /v1/refunds/{id}/cancel`), and checks its customers' pause
   scopes and open quotes. If the merchant cannot act promptly, pause `quotes` and `refunds` of its
   account meanwhile (routes are shared by every account of the mode, so never pause a route for
   one account); deposits keep being credited:

```sh
admin POST "/v1/admin/accounts/$ACCOUNT/pause" \
  '{"scopes":["quotes","refunds"],"reason":"<ticket>: key exposure under review"}'
```

## Done when

The merchant's requests with its new key succeed, a request with a revoked key answers
`401 api_key_invalid`, and every refund created since the exposure is confirmed or canceled.
