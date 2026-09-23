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

### Webhooks

- `deposit.credited` and `deposit.rejected` payloads now include `chain_id`, `state`
  (`credited` or `rejected`), and `route` (null when no route was selected); `chain_id` and
  `route` match the fields already on `deposit.confirmed`. This covers every producer, including
  the scanner's `unsupported_asset` rejection. The change is additive; existing fields are
  unchanged. See `docs/architecture.md` §12.
