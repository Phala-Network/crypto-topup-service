# Changelog

All notable changes to `crypto-topup-sdk` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the rules in `docs/sdk.md`; versions
follow [Semantic Versioning](https://semver.org/).

## Unreleased

### Added

- `TopupClient.list_pending_deposits` and the generated `list_pending_deposits` operation with
  `PendingDepositResponse` and `PendingDepositsResponse`: transfers to persistent addresses seen
  before finality. Display only; they are not credited.
- `RateLockResponse.payment` (`RateLockPayment`): the payment the checkout page should show (the
  consuming deposit, else the first qualifying payment, else the first), `seen` before finality
  or `finalized` once it is a deposit.

### Changed

- The service now rebuilds `@target-uri` from its configured public origin
  (`TOPUP_PUBLIC_ORIGIN`) instead of the `Host` and `X-Forwarded-Proto` headers, so requests
  signed for the public URL verify behind the dstack gateway (#77). The `http_message_signature`
  security scheme in `openapi.json` documents this. No SDK code changed.

## 0.1.0 - 2026-09-22

Generated from OpenAPI `info.version` 0.1.0.

### Added

- Generated `topup_client` package for the product and administrative API.
- RFC 9421 ed25519 request signing (`RequestSigner`, `SigningAuth`) with a random `nonce` per
  signature, and inbound verification (`verify_request`) in the service's profile.
- Standard Webhooks `v1a` verification (`verify_webhook`, `verify_webhook_signature`).
- Deposit-id, salt, and CREATE2 forwarder-address recomputation.
- `TopupClient` with idempotent, retrying helpers for accounts, deposit addresses, rotation,
  rate locks, deposits, limits, refunds, and attestation.
- `topup-sdk keygen` and `topup-sdk public-key` for credential issuance.
