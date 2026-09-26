# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

### Added

- `POST /v1/admin/reconciliation-blocks/{block_key}/lift {reason}` lifts a reconciliation block
  (`chain:{chain_id}` or `address:{address_id}`), which production could not do without a database
  owner session. Lifting is manual: the reconciler blocks again if the finding still reproduces.
  The `audit` row (`reconciliation_block.lift`) carries the reason and the removed block; a repeat
  returns the first lift, and a key that never blocked is `404`. `GET /v1/admin/report/daily`
  lists the active `reconciliation_blocks`.

- `POST /v1/admin/outbox/{event_id}/replay {reason}` queues an existing webhook event for
  delivery again with the same id and payload (audited as `outbox.replay`); a repeat while the
  event is due changes nothing. The product-signed support lookup
  (`GET /v1/products/{p}/deposits?tx_hash=|address=|lock_ref=`) lists each deposit's webhook
  `events` (`id`, `event_type`, `created_at`, `delivered_at`).

- `GET /v1/attestation` returns `operators`: for each configured chain, the flusher operator
  (`chain_id`, `operator_key_version` from the chain's current routes, `keyid` `operator/v{n}`,
  `address`) that needs `OPERATOR_ROLE` on the factory and native gas. `report_data` now binds
  them: `sha256(nonce ‖ settlement_pubkey ‖ record_1 ‖ … ‖ record_n)`, one 32-byte record per
  operator in list (ascending `chain_id`) order: `chain_id` (u64 big-endian),
  `operator_key_version` (u32 big-endian), and the 20 address bytes (architecture §14). With no
  operators the value is unchanged; a verifier that hashes only `nonce ‖ settlement_pubkey` must
  append the records. `operators` is optional in the schema so clients also parse responses from
  servers that predate it. `topup attest --route FILE` prints the same `operators` and
  `report_data` for those routes.

- `POST /v1/admin/products {slug, public_key, webhook_url}` (administrative API) issues a
  product with an `audit` row (`product.issue`); it replaces direct database registration. The
  key id and settlement URL still come only from the attested route, whose slug must be loaded.
  The same values return the same product with `200`; different values for an issued slug are
  `409 conflict`.

- `GET /v1/admin/report/daily` reports why sweeping or reconciliation stopped, which
  production otherwise shows only in logs: each route's `flush_planning` (`at`, `outcome`
  `planned`, `idle`, `operator_not_authorized`, `failed`, or `send_failed`, and the redacted
  `error`) from its latest scheduled planning run, and the report-level `reconciliation` (`at`
  and `failed_checks`, each with `check` and `error`) from the latest round. Both describe the
  serving process and are absent until its first run; both are optional in the schema.

- `GET /v1/admin/report/daily` returns `exposure_minor`, the global open rate-lock credit in
  destination minor units (#94). The field is optional in the schema so clients also parse reports
  from servers that predate it.

### Changed

- **Settlement conformance:** the `unknown_get` case now requires `404` for `GET` of an unknown
  settlement key and fails `200 {"status":"unknown"}`, which it used to accept. The service
  resends a settlement only after a `404` by key; the other answer made it poll without ever
  resending. `topup-conformance-reference --broken unknown-status` answers the old form and fails
  exactly that case.

### Removed

- **Breaking (administrative API):** `GET /v1/admin/report/daily` route entries no longer carry
  `exposure_minor`, `exposure_minor_reason`, `pnl_minor`, or `pnl_minor_reason` (#94). They were
  always null placeholders; route exposure now comes from the report-level `exposure_minor`, and
  PnL is not defined precisely enough in the design to compute. Allowed as a pre-GA exception:
  the endpoint is admin-only and no service has been deployed.

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
