# 422 payload mismatch

**Trigger:** the settlement endpoint answered `422` for an existing idempotency key, usually first
seen as `TopupDepositStateAgeExceeded` with `state:cleared`. The deposit's latest timeline
evidence is `error: "settlement_payload_mismatch"`, and the settlement is marked never to be
resent.

**Impact:** the service and the product disagree about the immutable payload of one settlement.
The deposit must not be resent or re-priced. Repeated mismatches point to restore or config
corruption on one product route.

## First steps

```sh
admin POST "/v1/admin/routes/$ROUTE/pause" '{"scopes":["settlement"]}'
```

Read the deposit (support lookup) and have the product preserve its original stored payload and
response under the incident.

## Decide

- Product's original payload equals the service's: the product's idempotency is faulty.
- Payloads differ but the chain evidence matches: investigate the restore or config version.
- Chain evidence differs: critical integrity incident; keep settlement paused.

## Fix

The product's fact and original payload are authoritative. Once the product confirms its record,
the service adopts it through its `GET`-first path. There is no way to clear the no-resend mark,
and settlement requests are never replayed.

## Done when

The deposit adopts the product's fact without a new `POST`, its stored valuation matches the
product's payload, no duplicate destination transaction exists, and every mismatch is classified
before settlement is resumed.
