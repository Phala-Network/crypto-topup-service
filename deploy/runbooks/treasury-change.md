# Treasury change

**Trigger:** a merchant, Phala Cloud's Finance included, moves a chain's treasury to another
address; an unexpected `account.treasury.pending` event, a change the merchant did not request;
or the `TopupTreasurySanctioned` alert.

**Impact:** treasuries are the accounts' own, set through the API per chain and mode with an
EIP-4361 proof (docs/architecture.md §9, "Treasuries"; design D10); the operator never sets one.
Every forwarder's address commits to its treasury (the clone's only immutable argument), so a
change affects only what is issued after it applies: new quotes, and the chain's network of every
deposit address of the account, which moves to a forwarder over the new treasury. Addresses issued
before keep paying the old treasury and are still watched and credited, and their deposits are
refunded from it: the merchant must keep control of an old treasury while customers may still pay
an old address. A live change applies 48 hours after it is proven; a test-mode change and a
chain's first treasury apply at once.

## Planned change (the merchant)

1. Request a challenge for the new address and sign it: an EOA with `personal_sign`; a Safe's
   owners as a Safe message, or on chain with `SignMessageLib` and the signature `0x`
   ([integration guide §1.6](../../docs/integration.md#16-treasuries)). The Safe must be deployed
   on that chain; ERC-6492 signatures are refused.
2. Submit it with `POST /v1/treasuries` before the challenge's `expires_at` (10 minutes for an
   EOA, 24 hours for a Safe). A live change answers `pending` with its
   `effective_at` and sends `account.treasury.pending` to every enabled endpoint of the mode.
3. At `effective_at` the time-lock worker applies it and sends `account.treasury.updated`; list
   the chain's treasuries with `GET /v1/treasuries?chain_id=…` (`active`, then `replaced`).

## Unrequested change (the merchant, then the operator)

The merchant cancels the pending change before it applies,
`POST /v1/treasuries/{id}/cancel` (`account.treasury.canceled`), and rolls its keys
([API key compromise](api-key-compromise.md)). If the key holder races it (proves the change again,
rolls the keys), the merchant asks the operator, who verifies the request with the recorded
contact and revokes the mode's keys with a recovery key; a pending change then stays cancellable
with the new key.

## Sanctioned treasury

The time-lock worker screens a treasury again when its change is due and every day while it is
current, with the chain's sanctions oracle on both providers (design §8). A due change whose
address a list now names is not applied: it is `canceled` with `cancellation_reason: sanctioned`
and `account.treasury.canceled`. A current treasury a list names raises `TopupTreasurySanctioned`
and pauses the account's `quotes` and `settlement` (audit action `pause`, actor
`system:treasury_screening`, and `account.updated`): no new address is issued and no deposit is
credited; deposits wait in `confirmed`.

1. **Compliance** reviews the listing (the alert names the `trs_` id and chain) and contacts the
   merchant through the recorded contact.
2. The merchant proves a new treasury on the chain (a live change waits 48 hours; the operator
   cannot shorten it).
3. Once the new treasury is `active` and Compliance clears the account, lift the pause:

```sh
admin POST "/v1/admin/accounts/$ACCOUNT/resume" \
  '{"scopes":["quotes","settlement"],"reason":"<ticket>: treasury replaced and reviewed"}'
```

   Addresses issued over the listed treasury keep paying it; its funds are the merchant's
   compliance matter (design §17, item 2).

## Done when

`GET /v1/treasuries` shows the intended treasury `active` on the chain and no unintended change
`pending`; a new quote on the chain carries it as `treasury`; the deposit addresses' network on the
chain pays it.
