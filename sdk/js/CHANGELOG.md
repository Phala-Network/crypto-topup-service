# Changelog

## 0.1.0 (unreleased)

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
