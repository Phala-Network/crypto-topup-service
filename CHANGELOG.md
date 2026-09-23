# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

### API

- Rate locks carry an optional `payment` object: the first payment to the lock address,
  `status: "seen"` while it is above `finalized` (with `confirmations`, `amount_within_tolerance`,
  `in_time`, `supported`, and `estimated_final_at`, which is block time plus 15 minutes) and
  `"finalized"` once it is a deposit (`deposit_id` locates it).
- New `GET /v1/products/{p}/accounts/{ext}/pending-deposits` lists transfers to the account's
  persistent addresses seen above `finalized`. They are not deposits and are not credited; once
  final they leave this list and appear under `deposits`, and a reorg can remove them.

See `docs/architecture.md` §8 and §12. Both are display only: crediting is unchanged and still
happens only from two-provider finalized data.

### Webhooks

- New event `deposit.pending`, sent at most once per chain event when a transfer to a watched
  address is first seen above `finalized`. Its payload is marked `provisional: true`; it never
  changes a balance, and the transfer may still disappear in a reorg. Receivers that do not
  handle it must ignore it, as with any unknown event type.
- `deposit.credited` and `deposit.rejected` payloads now include `chain_id`, `state`
  (`credited` or `rejected`), and `route` (null when no route was selected); `chain_id` and
  `route` match the fields already on `deposit.confirmed`. This covers every producer, including
  the scanner's `unsupported_asset` rejection. The change is additive; existing fields are
  unchanged. See `docs/architecture.md` §12.
