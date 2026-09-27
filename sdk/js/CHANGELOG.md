# Changelog

## 0.1.0 (unreleased)

### Added

- `createCheckout`: follows a quote's public view (`GET /v1/quotes/{id}?client_secret=…`) and
  exposes the payer-facing status.
- `watchWallets` (EIP-6963, `window.ethereum` fallback) and `payWithWallet`: pay a quote's EIP-681
  ERC-20 transfer from a browser wallet, switching or adding the chain.
- `@phala/crypto-topup/react`: `<CryptoTopupCheckout>` with wallet, QR code, and manual payment,
  live status, and Stripe-style `appearance`; `useCheckout`.
