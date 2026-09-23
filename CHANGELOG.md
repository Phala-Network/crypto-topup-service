# Changelog

Integrator-visible changes to the HTTP API and webhook payloads. Additive fields are not breaking;
webhook receivers must ignore unknown fields.

## Unreleased

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
