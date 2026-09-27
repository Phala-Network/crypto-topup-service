# Changelog

All notable changes to `@phala/pay` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-27

### Added

- `PhalaPay({ apiBase })`: `retrieveQuote(clientSecret)` reads a quote's public view
  (`GET /v1/quotes/{id}?client_secret=…`) and `checkout(clientSecret)` follows it, exposing the
  payer-facing status (also `createCheckout` and `retrieveQuote`).
- `watchWallets` (EIP-6963, `window.ethereum` fallback) and `payWithWallet`: pay a quote's EIP-681
  ERC-20 transfer from a browser wallet, switching or adding the chain.
- `@phala/pay/react`: `<Checkout>` with wallet ("Pay with crypto", `buttonText`), QR code, and
  manual payment, live status, and Stripe-style `appearance`; `useCheckout`.
- `formatTokenAmount(quote, locale?)` shows the exact token amount grouped for the locale and
  without trailing zeros (`1,273.9185`); `tokenAmount(quote)` is the plain decimal a wallet
  accepts, which `<Checkout>`'s "Exact amount" copies.

[unreleased]: https://github.com/Phala-Network/phala-pay/compare/sdk-js-v0.1.0...HEAD
[0.1.0]: https://github.com/Phala-Network/phala-pay/releases/tag/sdk-js-v0.1.0
