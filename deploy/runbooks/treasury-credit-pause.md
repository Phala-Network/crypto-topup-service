# Treasury crediting pause

**Trigger:** a merchant reports that one of its treasuries is compromised (for example a former
treasury's key or a Safe owner's key leaked), or Compliance or the operator learns that funds
reaching a treasury are no longer the merchant's to control. The service raises no alert for this:
it cannot see who holds a treasury's keys.

**Impact:** every forwarder issued over a treasury pays only that treasury, forever (design D3),
so payments customers still send to those addresses end up where the thief can sweep them. The
service cannot redirect or freeze funds (it sends no transactions and has no contract role); what
it can stop is **crediting**: while crediting of the treasury is paused, deposits to every
forwarder over its address (on that chain, in that mode) stay `pending` (`confirmed` in the
deposit view) and no `deposit.credited` is sent, so the merchant does not credit customers for
funds it may never receive. Deposits already credited are unchanged, and deposits to other
treasuries are credited as before. A pause is resumable: once lifted, the held deposits are
credited, each with its `deposit.credited`.

The merchant and the operator each have their own pause, and neither lifts the other's
(`crediting_paused_by` on the treasury lists `merchant`, `operator`, or both), as with the
account's `paused_scopes`. Every change is audited and sent as `treasury.updated` to every enabled
endpoint of the mode.

## The merchant pauses it

With a secret key (restricted keys cannot), the merchant pauses the treasury's crediting itself,
then moves its current treasury if the compromised one is current
([Treasury change](treasury-change.md)):

```sh
curl -sS -X POST -H "Authorization: Bearer $SECRET_KEY" "$BASE_URL/v1/treasuries/$TREASURY_ID/pause"
```

## The operator pauses it

When the merchant cannot act, or Compliance requires it:

1. Verify the request with the account's recorded contact over a second channel (the `contact`
   of `POST /v1/admin/accounts`), as for [API key compromise](api-key-compromise.md). Never act on
   the request's own channel alone.
2. Find the treasury's `trs_` id from the merchant, or from the `treasury.*` event it names.
3. Pause crediting, with the ticket in the reason:

```sh
admin POST "/v1/admin/accounts/$ACCOUNT/treasuries/$TREASURY_ID/pause" \
  '{"reason":"<ticket>: treasury reported compromised by <how the contact confirmed>"}'
```

   The answer is the treasury with `crediting_paused: true`; the `audit` row `treasury.updated`
   carries actor `admin`.
4. Tell the contact that payments to addresses over that treasury are held, that customers should
   be shown the account's current deposit address (a treasury change moves every deposit address
   network, design D10), and that held payments are the merchant's to settle with its customers
   and Compliance.

## Resume

Once the treasury is back under the merchant's control, or Compliance has decided how held funds
are handled, lift the operator's pause (the merchant lifts its own with
`POST /v1/treasuries/{id}/resume`):

```sh
admin POST "/v1/admin/accounts/$ACCOUNT/treasuries/$TREASURY_ID/resume" \
  '{"reason":"<ticket>: treasury control confirmed"}'
```

## Done when

`GET /v1/treasuries/{id}` shows the intended `crediting_paused_by`; while paused, a payment to an
address over it stays `pending` in `GET /v1/deposits`; after resuming, the held deposits are
`credited`.
