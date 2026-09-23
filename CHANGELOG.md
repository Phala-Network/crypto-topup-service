# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

### Administrative API

- `GET /v1/admin/report/daily` returns `exposure_minor`, the global open rate-lock credit in
  destination minor units. Route entries no longer carry the always-null `exposure_minor`,
  `exposure_minor_reason`, `pnl_minor`, and `pnl_minor_reason` fields.

### Webhooks

- `deposit.credited` and `deposit.rejected` payloads now include `chain_id`, `state`
  (`credited` or `rejected`), and `route` (null when no route was selected); `chain_id` and
  `route` match the fields already on `deposit.confirmed`. This covers every producer, including
  the scanner's `unsupported_asset` rejection. The change is additive; existing fields are
  unchanged. See `docs/architecture.md` §12.
