# Reconciliation after a restore

**Trigger:** the database was restored from backup ([RESTORE.md](../RESTORE.md)); merchants get
`503 service_restoring` with `Retry-After` on every request with an API key, reads included;
`admin GET /v1/admin/restore` shows `"frozen": true`.

**Why:** a restore brings back the database as of its **restore point** (the newest heartbeat in
it, at most the RPO before the loss), not the business as it was. Everything after the restore
point is lost, and some of it matters beyond the database:

- an API key the merchant revoked or rolled works again, secret or restricted;
- a treasury change the merchant canceled is pending again and would apply at its `effective_at`;
- a treasury whose crediting the merchant paused credits again (or a resume is undone);
- a webhook endpoint the merchant deleted receives events again;
- a deposit address given to a customer is unknown, so payments to it are not credited;
- a quote given to a customer is unknown (its address derives from its random id), so a payment
  to it is not found;
- an event the merchant received is gone: re-derived from the chain, a spot-priced deposit would
  be re-valued, and its `deposit.credited` carries the same event id with another `amount`, and
  its refunds and reversal would reference the re-valued amount.

So the service starts **frozen** after a restore (architecture §14): the admin API, `/healthz`, the
scanner, and the reconciler run; every merchant request with an API key answers
`503 service_restoring`, reads included, since a key revoked after the restore point is valid in
the restored database; nothing credits, settles, expires a quote, applies a treasury change,
verifies a refund, or delivers an event. The
freeze is a database row, so it holds from the restore-check instance through the upgrade to the
service compose, and a restore that booted straight into the service compose freezes it too (its
PostgreSQL timeline is new). Only `POST /v1/admin/restore/unfreeze` lifts it, once every chain is
rescanned.

Work through the steps in order, with the runbook environment of the [README](README.md#environment)
and `BASE_URL` set to the instance's origin: `$RESTORE_URL` on the restore-check instance (after
[Restore](../RESTORE.md#restore) step 5), `https://$TOPUP_DOMAIN` once resumed. Every write below
but the discard of step 6 needs the freeze (`400 restore_not_frozen` otherwise), and each writes
`audit`. Steps 2 to 5 can run on
the restore-check instance, before the service resumes; do the security steps as early as possible.

## 1. Read the freeze and the restore point

```sh
admin GET /v1/admin/restore | tee restore-status.json | jq '{frozen, restore, rescan}'
```

`restore.restore_point` (Unix seconds) is where the lost window starts; `restore.detected_by` is
`restore_check` or `timeline`. Record both in the incident.

## 2. Ask every merchant for its records since the restore point

Through [incident communication](incident-communication.md), send each account's recorded contact
the restore point and ask for what it did or received after it, from its own records (its webhook
receiver's store, its database, or an `export_account` taken before the loss):

1. API keys it created, revoked, or rolled, secret and restricted: the key's `id` (`key_…`), or its
   prefix and last four characters (`ppay_sk_live_…abcd`, `ppay_rk_live_…abcd`);
2. the latest `treasury` object of each treasury it received an event about (`treasury.created`,
   `.updated`, `.canceled`), with its `status` and `crediting_paused_by`;
3. webhook endpoints it deleted (`we_…`);
4. deposit addresses it received: `client_reference_id`, `address`, and `id` (`da_…`) or
   `version`, and a `client_secret` it holds for one;
5. quotes it created: the quote object (`id`, `client_reference_id`, `chain_id`, `asset`,
   `amount`, `amount_atomic`, `exchange_rate`, `address`, `created`, `expires_at`, `metadata`) and
   its `client_secret`, if it kept it;
6. the delivery of every `deposit.credited`, `deposit.rejected`, and `deposit.reversed` event it
   received, as its receiver got it: the raw body, byte for byte, with its `webhook-id`,
   `webhook-timestamp`, and `webhook-signature` headers. Only a delivery the service signed is
   imported, so a re-serialized body or a missing header cannot be used.

Tell it what it cannot produce is lost: a quote it has no record of is not found, so a payment to
it is not credited (the chain cannot name it: its address derives from its random id); a spot
deposit whose delivery it has no record of is re-valued, and its refunds and reversal reference the
re-valued amount; a quote whose `deposit.credited` it has no record of is credited at spot, since
its own record of the quote's price is not proof the service issued it
([design §13](../../docs/design/multi-tenant.md#13-operations)). Its API keys answer `503` until the unfreeze
(step 7).

Refunds it created or marked paid after the restore point are gone too: after the unfreeze it
creates them again and marks them paid with the same transaction. Their `deposit.refunded` carries
a new id, but the deposit's cumulative `amount_refunded` is the same, so the balance rule of the
[integration guide](../../docs/integration.md#the-balance-rule-and-event-ordering) takes nothing
back twice.

Keys and endpoints it created after the restore point are gone; it creates them again after the
unfreeze (a merchant left without a working key gets a recovery key,
[API key compromise](api-key-compromise.md#recovery-by-the-operator)).

## 3. Re-apply the security changes

**Required before the unfreeze.** While frozen no API key authenticates; the unfreeze lets every
key the restored database holds as valid read and write again, including one the merchant revoked
after the restore point. Revoke again every key the merchant revoked or rolled:

```sh
admin POST /v1/admin/restore/api_keys/revoke \
  '{"account":"acct_…","prefix":"ppay_sk_live_","last4":"abcd","reason":"INC-…: revoked at 10:02 PDT"}'
```

or with `"id":"key_…"` instead of `prefix` and `last4`. The answer is the key with
`"status": "revoked"`; after the unfreeze a request with it answers `401`. `400` names several keys
with the same prefix and last four (send the `id`), or `last_api_key` (issue a recovery key with
`revoke_existing`). A merchant that has not answered keeps every restored key valid at the
unfreeze: revoke on its behalf any key it reported compromised through other channels, or leave
the account paused until it answers.

Compare the treasuries with what the merchant received, cancel again what it canceled, and pause
or resume crediting again as it last did:

```sh
admin POST /v1/admin/restore/treasuries/verify \
  '{"account":"acct_…","livemode":true,"treasuries":[{"id":"trs_…","status":"canceled","chain_id":1,"address":"0x…","crediting_paused_by":["merchant"]}],"reapply":true,"reason":"INC-…"}' | jq
```

Each `result` is `matches`, `canceled` (canceled again now), `cancellation_lost` (without
`reapply`), `missing` (proven after the restore point: the merchant proves it again after the
unfreeze), or `differs` (a change that applied after the restore point: it applies again at its
`effective_at` after the unfreeze; anything else, escalate). Each `crediting` (when
`crediting_paused_by` was sent) is `matches`, `paused` or `resumed` (applied again now, announced
as `treasury.updated`), or `pause_lost` or `resume_lost` (without `reapply`). No treasury change
applies and nothing is credited while frozen, so none takes effect before this step. Only the
merchant's own pause is compared: re-apply the operator's own treasury pauses made after the
restore point from the incident record
(`admin POST "/v1/admin/accounts/$ACCOUNT/treasuries/$TREASURY_ID/pause" '{"reason":"…"}'`).

Delete again every endpoint the merchant deleted, before deliveries resume:

```sh
admin POST /v1/admin/restore/webhook_endpoints/delete \
  '{"account":"acct_…","livemode":true,"id":"we_…","reason":"INC-…"}'
```

## 4. Re-issue the deposit addresses given out after the restore point

A deposit address's salt is derived from the account, mode, `client_reference_id`, and version
([design §5a](../../docs/design/multi-tenant.md#5a-deposit-addresses-d16)), so the service issues
the same address again from the merchant's record. Re-apply treasury changes first: a network's
address is derived over the chain's current treasury.

```sh
admin POST /v1/admin/restore/deposit_addresses \
  '{"account":"acct_…","livemode":true,"client_reference_id":"team-42","address":"0x…","id":"da_…","reason":"INC-…"}' | jq
```

The answer's `deposit_address` has the merchant's `address` and `id`; the versions between the
restored latest one and it are issued retired, as the rotations left them. Each new network is
backfilled from the restored cursor, so the rescan credits payments made to it since. `400` means
the address is not the customer's over the account's current treasuries: check the treasury, then
the merchant's record. The chain alone cannot name these customers: a salt is a hash of the
`client_reference_id`.

A merchant can also re-register an address itself after the unfreeze: `POST /v1/deposit_addresses`
returns version 1 identically, and each `POST /v1/deposit_addresses/{id}/rotate` the next version.
Payments made to it meanwhile are credited once it is registered, but only from the chain's cursor
at that time; re-issue here so nothing is missed.

Re-issue every quote the merchant created after the restore point the same way. Its address salt
is derived from the account, `client_reference_id`, and `qt_` id ([architecture
§9](../../docs/architecture.md#9-quotes)), so only the quote's own address over the chain's current
treasury is accepted:

```sh
admin POST /v1/admin/restore/quotes \
  '{"account":"acct_…","livemode":true,"id":"qt_…","client_reference_id":"team-42","chain_id":1,"asset":"pha","amount":1000,"amount_atomic":"…","exchange_rate":"0.10000000","address":"0x…","created":1790000000,"expires_at":1790000900,"metadata":{},"client_secret":"qt_…_secret_…","reason":"INC-…"}' | jq
```

Only the address is checked: the terms are stored as the merchant recorded them, whatever the
route issues today, and never applied. The locked price is the merchant's record, not the
service's, so a payment to the quote is credited at spot, unless the merchant's delivered
`deposit.credited` for it (step 5) carries the quote's credit; and the quote's `expires_at` is the
restore's detection at the latest, so its payment page shows it expired instead of asking for a
payment at a price that is not honoured. Send the quote's `client_secret` when the merchant holds
it: only a secret the service issued for that `qt_` id is accepted (its tag proves it), and it is
kept, so the payer's page reads the quote again; without it the quote is re-issued and its payment
found all the same, and a repeat with the secret, once the merchant finds it, adds it. `reissued` is `false` when the quote exists already for the customer at the
address. Its address is backfilled from the restored cursor, so the rescan finds a payment made to
it. `400` means the address is not the quote's over the current treasury (check the treasury, then
the record), the `client_secret` is not the quote's, or no route has the chain and asset. A quote
nobody reports stays lost.

The same `client_secret` field on the deposit address re-issue above (with its `id`) keeps a secret
the merchant holds for the address.

## 5. Import the events the merchant received

```sh
admin POST /v1/admin/restore/events '{"deliveries":[{"webhook_id":"evt_…","webhook_timestamp":"1790000005","webhook_signature":"v1a,…","body":"{\"id\":\"evt_…\",…}"}],"reason":"INC-…"}' | jq
```

Up to 100 deliveries per request, each as the merchant's receiver got it: `body` is the raw
request body as a string, byte for byte. Only what the service signed is imported: the
`webhook_signature` must verify over `webhook_id`, `webhook_timestamp`, and `body` with one of the
account's webhook keys in the event's mode (every version up to the restored current one, and the
next, for a roll lost with the restore). Each event is stored as the event it is, with no delivery:
when the rescan re-derives its deposit, the event is recorded already and nothing is sent again
with another body. The credit a `deposit.credited` or `deposit.reversed` carries is kept, and the
deposit is valued at it (amount, exchange rate, price source, valuation time), not re-valued, so its
refunds and reversal reference what the merchant was told. `imported`; `matches` (recorded
already, same body); `mismatch` (recorded already with another body, which is kept; record it).
`400` names a delivery whose signature does not verify, whose event is not a re-derived deposit
event, or whose id is not the one its type and deposit derive; nothing of that request is
imported. `503` means the webhook keys cannot be derived; retry.

## 6. Resume and wait for the rescan

On a restore-check instance, [resume](../RESTORE.md#resume): the service boots frozen. The scanner
rescans each chain from its restored cursor and the reconciler runs; deposits are recorded, not
credited. Wait until every chain is rescanned:

```sh
admin GET /v1/admin/restore | jq '.rescan[] | {chain_id, restored_block, scanned_block, pending_backfills, blocked, complete}'
```

A chain is `complete` once it finalized past the moment the restore was detected with every
issued address backfilled. A chain `blocked` by reconciliation is left out: it credits nothing
until its block is lifted ([Chain frozen](chain-frozen.md)).

Once the rescan has recorded the deposits, check the imported events against them:

```sh
admin GET /v1/admin/restore | jq '.delivered_events.findings[] | select(.status == "contradicted")'
```

A `contradicted` finding is a deposit whose recorded transfer (account, mode, chain, transaction,
recipient, token, sender, or amount) is not the one the signed delivery names. The service never
delivers such an event, so escalate: the deposit is held, not valued or credited, and its confirm
step retries as an invariant violation until the operator decides. Once the chain is confirmed
right, discard the delivered credit, audited; the deposit is then valued from the chain as any
other, and the difference is settled with the merchant:

```sh
admin POST /v1/admin/restore/delivered_credits/discard '{"deposit":"dep_…","reason":"INC-…: chain shows …; settled with the merchant"}'
```

It works frozen or not, since the confirm step can also find a contradiction after the unfreeze.

## 7. Unfreeze

Only when steps 3 to 5 are done for every account that answered, every key a merchant revoked or
rolled after the restore point is revoked again (step 3), and every chain is `complete`. The
unfreeze is when merchant keys authenticate again:

```sh
admin POST /v1/admin/restore/unfreeze \
  '{"reason":"INC-…: reconciled, signed off by …","security_changes_reapplied":true,"deposit_addresses_reissued":true,"quotes_reissued":true,"delivered_events_imported":true}'
```

`400 restore_rescan_incomplete` means a chain is not rescanned yet. The reason and checklist are
recorded in the restore and in `audit`; crediting, settlement, quote expiry, treasury changes,
refund verification, and event delivery resume, and merchants' API keys work again, reads and
writes. Each checklist item is a step: `security_changes_reapplied` step 3,
`deposit_addresses_reissued` and `quotes_reissued` step 4, `delivered_events_imported` step 5.

## 8. After the unfreeze

```sh
admin GET /v1/admin/restore | jq '.delivered_events'
```

Each finding is an imported event whose deposit the ledger does not hold as delivered:
`pending` until the rescan re-derives and values it; `contradicted` while its recorded transfer is
not the delivered one (step 6); `mismatch` when the ledger's token amount or credit differs from
what the merchant received. A delivered credit is carried into the ledger, so a `mismatch` follows
only a discarded contradiction or an event that carries no credit. The merchant keeps its
delivered credit and is never sent another; record each mismatch, both amounts, and the deposit in
the incident and settle it with the merchant. A `pending` finding that stays after the rescan is a
deposit the chain does not show: escalate. One exception: a deposit the finality watch reversed
because another transfer took its receipt position, and its successor (the successor's
`replaces` names it), when the backup predates both. The rescan records the final transfer under
the reversed deposit's id, so that deposit's imported credit contradicts the transfer it now holds:
it shows as `contradicted` and is held, and the successor's events stay `pending`. Record the two as
one incident, settle what the merchant applied with it, then discard the held deposit's delivered
credit (step 6). (A backup
holding the reversed deposit holds its successor too: both commit in one transaction. Should one
ever lack it, the rescan records the transfer as a new deposit with `replaces` `null`, and the
quote is not handed over: link the two by their transaction hash.)

## Done when

`frozen` is `false`, every chain was `complete` at the unfreeze, no `delivered_events` finding is
`pending` or `contradicted`, each `mismatch` and discarded credit is recorded and settled, and
every merchant confirmed that its keys, treasuries, endpoints, deposit addresses, and quotes are as
it left them.
