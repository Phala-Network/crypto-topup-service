# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

### HTTP API

- Removed the unreachable `501` response from `/v1/attestation` and the `work_package` error field
  from `openapi.json`; both belonged only to the pre-C11 placeholder, and production never
  returned them (#90).
- Rate locks carry an optional `payment` object, chosen by the lock consumption rule: the
  deposit that consumed the lock, otherwise the first payment that would consume it, otherwise
  the first payment. `status` is `"seen"` while it is above `finalized` (with `confirmations`
  and `estimated_final_at`, block time plus 15 minutes) and `"finalized"` once it is a deposit
  (`deposit_id` locates it); `supported`, `in_time`, and `amount_within_tolerance` describe it
  against the lock and are false on a cancelled lock.
- New `GET /v1/products/{p}/accounts/{ext}/pending-deposits` lists transfers to the account's
  persistent addresses seen above `finalized`. They are not deposits and are not credited; once
  final they leave this list and appear under `deposits`, and a reorg can remove them.

See `docs/architecture.md` §8 and §12. Both are display only: crediting is unchanged and still
happens only from two-provider finalized data.

### Webhooks

- New event `deposit.pending`, sent at most once per chain event when a non-zero transfer of a
  routed token to a watched address is first seen above `finalized`. Its payload is marked
  `provisional: true`; it never changes a balance, and the transfer may still disappear in a
  reorg. It may arrive after `deposit.credited` for the same deposit, so act on fetched state,
  not event order. Receivers that do not handle it must ignore it, as with any unknown event
  type.
- `deposit.credited` and `deposit.rejected` payloads now include `chain_id`, `state`
  (`credited` or `rejected`), and `route` (null when no route was selected); `chain_id` and
  `route` match the fields already on `deposit.confirmed`. This covers every producer, including
  the scanner's `unsupported_asset` rejection. The change is additive; existing fields are
  unchanged. See `docs/architecture.md` §12.

### Rate locks

- A lock now expires by chain time: `rate_lock.expired` is emitted only once the finalized chain
  has passed `expires_at` and no payment mined inside the window awaits confirmation, so a payment
  made in the last minutes of the window is consumed at the lock price and never reported as
  expired. Until then `GET …/rate-locks/{ref}` returns `status: open` with
  `remaining_seconds: 0`; expiry events arrive about 15 minutes after `expires_at`. See
  `docs/architecture.md` §9.
- `DELETE …/rate-locks/{ref}` on a lock whose payment window has closed but which has not yet
  expired now answers `409` with the new error code `window_closed` ("payment window has
  closed") instead of `conflict`. `conflict` remains for consumed or expired locks.
- `GET …/limits` `reset_at` is the earliest payment-window close among open reserved locks. It can
  be in the past: exposure is released only at chain finality, about 15 minutes later.
