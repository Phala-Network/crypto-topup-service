# @phala/pay

Browser checkout for Phala Pay: a framework-agnostic client for a quote's public
view, and a React component that lets the payer pay from a browser wallet, by QR code, or manually,
with live status. It is the browser half of the flow; your backend creates the quote with the
Python SDK (`phala-pay`) and fulfils from the signed `deposit.credited` webhook.

## Install

```sh
npm install @phala/pay viem
```

Peer dependencies: `viem` 2, and `react` 18 or 19 for `@phala/pay/react`. Nothing else:
wallets are found with EIP-6963 (with a `window.ethereum` fallback), not wagmi or WalletConnect.

Until the first npm release, install from the repository with pnpm. The package builds itself
on install, which pnpm allows only for listed git dependencies: the first `pnpm add` stops and
prints an `allowBuilds` entry naming the exact commit; add it to `pnpm-workspace.yaml` and run the
same command again.

```sh
pnpm add "github:Phala-Network/phala-pay#main&path:/sdk/js"
```

```yaml
# pnpm-workspace.yaml: the entry pnpm printed
allowBuilds:
  "@phala/pay@https://codeload.github.com/Phala-Network/phala-pay/tar.gz/<commit>#path:/sdk/js": true
```

## Quickstart

1. Your backend creates a quote (`POST /v1/quotes`, signed with the product key) and returns only its
   `client_secret` to the signed-in user's browser.
2. Render the checkout with it:

```tsx
"use client";
import { Checkout } from "@phala/pay/react";

export function TopUp(props: { clientSecret: string; onPaid: () => void; onRetry: () => void }) {
  return (
    <Checkout
      clientSecret={props.clientSecret}
      apiBase="https://pay.example.com"
      onSuccess={props.onPaid}
      onExpire={props.onRetry}
      buttonText="Pay with crypto"
    />
  );
}
```

3. Credit the account when your webhook endpoint receives `deposit.credited`. `onSuccess` is for the
   UI only: the browser is not a trusted source of payment.

The component reads `GET /v1/quotes/{id}?client_secret=…` every three seconds. That endpoint is
public (`Access-Control-Allow-Origin: *`) and shows only what the payer needs: the amount, the
token, the chain, the deposit address, the EIP-681 payment request, the expiry, and the payment
status. Keep the client secret out of logs and URLs you share; anyone holding it can see that view.

### Statuses

| `status` | Shown |
|---|---|
| `loading` | Loading payment details |
| `waiting` | Payment options, the exact amount, and the time left |
| `seen` | Payment in a block, with its confirmations (a reorg can still remove it) |
| `confirming` | Payment final, being valued and screened |
| `credited` | Credited; `onSuccess` is called once |
| `rejected` | Final but will not be credited; the payer contacts support |
| `expired`, `canceled` | The address is hidden; `onExpire` is called once |
| `error` | The client secret is not valid |

The payment options disappear once a payment is seen, and at `expires_at`. A payment of a different
amount, or after expiry, is still credited, at the market price instead of the quote's.

## Appearance

```tsx
<Checkout
  clientSecret={clientSecret}
  apiBase={apiBase}
  appearance={{
    theme: "dark",
    variables: {
      colorPrimary: "#cdfa50",
      accessibleColorOnColorPrimary: "#161616",
      borderRadius: "12px",
      fontFamily: "Inter, sans-serif",
    },
  }}
/>
```

| Variable                        | CSS custom property                      | Light default | Dark default | Use                                          |
| ------------------------------- | ---------------------------------------- | ------------- | ------------ | -------------------------------------------- |
| `colorPrimary`                  | `--pp-color-primary`                     | `#0f62fe`     | `#78a9ff`    | Pay button, selected tab, links, focus ring  |
| `accessibleColorOnColorPrimary` | `--pp-accessible-color-on-color-primary` | `#ffffff`     | `#ffffff`    | Text on a `colorPrimary` background (button) |
| `colorBackground`               | `--pp-color-background`                  | `#ffffff`     | `#161616`    | Background                                   |
| `colorText`                     | `--pp-color-text`                        | `#1a1a1a`     | `#f4f4f4`    | Text                                         |
| `colorTextSecondary`            | `--pp-color-text-secondary`              | `#5c5f66`     | `#a8a8a8`    | Labels and hints                             |
| `colorBorder`                   | `--pp-color-border`                      | `#d9dce1`     | `#393939`    | Borders                                      |
| `colorDanger`                   | `--pp-color-danger`                      | `#c62828`     | `#ff8389`    | Errors, expired and rejected states          |
| `colorSuccess`                  | `--pp-color-success`                     | `#1b7f3b`     | `#42be65`    | Credited state                               |
| `fontFamily`                    | `--pp-font-family`                       | system UI     | system UI    | Font                                         |
| `borderRadius`                  | `--pp-border-radius`                     | `8px`         | `8px`        | Corner radius                                |

Each variable is also a CSS custom property on `.pp-root`, so a stylesheet can set it too. With a
light `colorPrimary`, set a dark `accessibleColorOnColorPrimary` so the button label stays
readable.

## Without React

```ts
import { PhalaPay, payWithWallet, watchWallets, type Wallet } from "@phala/pay";

const pay = new PhalaPay({ apiBase: "https://pay.example.com" });
const quote = await pay.retrieveQuote(clientSecret); // one read of the public view
const checkout = pay.checkout(clientSecret); // or follow it until it settles
const unsubscribe = checkout.subscribe(({ status, quote, error }) => render(status, quote, error));

const stop = watchWallets((wallets) => showWalletButtons(wallets));

async function onWalletClick(wallet: Wallet) {
  const { quote } = checkout.getState();
  if (quote !== null) {
    // Resolves with the transaction hash once the wallet broadcast the transfer.
    showTransaction(await payWithWallet(wallet.provider, quote));
    await checkout.refresh();
  }
}

// When leaving the page:
stop();
unsubscribe();
checkout.destroy();
```

`payWithWallet` connects, switches the wallet to the quote's chain (adding Ethereum, Sepolia,
Base, or Base Sepolia when the wallet lacks it), and sends the ERC-20 `transfer` stated by the
quote's `payment_uri`, after checking that it pays exactly `amount_atomic` to `address`.

## Development

```sh
pnpm install
pnpm run check   # typecheck, lint, unit tests, build
pnpm run e2e     # Playwright against Anvil; needs Foundry (anvil, forge) on PATH
pnpm run e2e:docker  # the same, with Chromium from Playwright's image (as CI runs it)
```

The end-to-end tests start Anvil as Sepolia, deploy a test token, and pay a quote from a mocked
EIP-6963 wallet backed by the node, then decode the QR code and exercise the manual details.

## Releases

A tag `sdk-js-v<version>` matching `package.json` publishes to npm from the Release SDKs workflow
(environment `npm`) with trusted publishing and provenance. See [CHANGELOG.md](CHANGELOG.md).
