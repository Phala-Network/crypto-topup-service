# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

### Added

- `GET /v1/admin/report/daily` returns `exposure_minor`, the global open rate-lock credit in
  destination minor units (#94). The field is optional in the schema so clients also parse reports
  from servers that predate it.

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

### Webhooks

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
