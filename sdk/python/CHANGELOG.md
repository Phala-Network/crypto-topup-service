# Changelog

All notable changes to `crypto-topup-sdk` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/) and the rules in `docs/sdk.md`; versions
follow [Semantic Versioning](https://semver.org/).

## Unreleased

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
