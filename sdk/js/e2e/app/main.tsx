import { StrictMode, useState } from "react";
import { createRoot } from "react-dom/client";
import { createWalletClient, custom, isAddress } from "viem";
import { Checkout } from "../../dist/react/index.js";
import type { EthereumProvider } from "../../dist/index.js";

const params = new URLSearchParams(window.location.search);
// `?wallet_client=<account>` passes the test wallet as the page's own viem client, as wagmi would.
const account = params.get("wallet_client");
const testWallet = (window as { testWallet?: EthereumProvider }).testWallet;
const walletClient =
  account !== null && isAddress(account) && testWallet !== undefined
    ? createWalletClient({ account, transport: custom(testWallet) })
    : undefined;

function App() {
  const [events, setEvents] = useState<string[]>([]);
  return (
    <main>
      <Checkout
        clientSecret={params.get("client_secret") ?? ""}
        expectedAddress={params.get("expected_address") ?? ""}
        apiBase={params.get("api_base") ?? ""}
        pollInterval={500}
        walletClient={walletClient}
        onSuccess={() => setEvents((e) => [...e, "success"])}
        onExpire={() => setEvents((e) => [...e, "expire"])}
      />
      <p data-testid="events">{events.join(",")}</p>
    </main>
  );
}

const root = document.getElementById("root");
if (root !== null) {
  createRoot(root).render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}
