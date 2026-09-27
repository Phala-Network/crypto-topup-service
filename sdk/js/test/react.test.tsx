import { act, cleanup, render, screen, within } from "@testing-library/react";
import { userEvent } from "@testing-library/user-event";
import { createWalletClient, custom } from "viem";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Checkout } from "../src/react/index.js";
import type { CheckoutState, ClientQuote, EthereumProvider } from "../src/index.js";
import { ADDRESS, API_BASE, CLIENT_SECRET, TOKEN, quote } from "./fixtures.js";

const NOW = (quote().expires_at - 14 * 60 - 32) * 1000;
let served: ClientQuote;

beforeEach(() => {
  vi.useFakeTimers({ now: NOW, shouldAdvanceTime: true });
  served = quote();
  vi.stubGlobal("fetch", () => Promise.resolve(Response.json(served)));
});
afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

async function renderCheckout(props: Partial<Parameters<typeof Checkout>[0]> = {}) {
  const view = render(
    <Checkout clientSecret={CLIENT_SECRET} apiBase={API_BASE} pollInterval={1000} {...props} />,
  );
  await screen.findByText("Waiting for your payment");
  return view;
}

async function poll() {
  await act(() => vi.advanceTimersByTimeAsync(1000));
}

describe("Checkout", () => {
  it("states the exact amount, the network, and the time left", async () => {
    await renderCheckout();
    expect(screen.getByText("100.502512562814070352 PHA")).toBeDefined();
    expect(screen.getByText("$25.00 top-up · Sepolia")).toBeDefined();
    expect(screen.getByText(/exactly 100.502512562814070352 PHA/)).toBeDefined();
    expect(screen.getByLabelText("Time left to pay").textContent).toBe("14:32");
    expect(screen.getByRole("status").getAttribute("aria-live")).toBe("polite");
  });

  it("offers the three payment methods as keyboard-operable tabs", async () => {
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    await renderCheckout();
    const tabs = screen.getAllByRole("tab");
    expect(tabs.map((t) => t.textContent)).toEqual(["Browser wallet", "QR code", "Send manually"]);
    expect(screen.getByRole("tabpanel").textContent).toMatch(/No browser wallet found/);

    tabs[0]?.focus();
    await user.keyboard("{ArrowRight}");
    expect(document.activeElement).toBe(screen.getByRole("tab", { name: "QR code" }));
    const qr = within(screen.getByRole("tabpanel")).getByRole("img");
    expect(qr.getAttribute("aria-label")).toBe("Payment request for 100.502512562814070352 PHA");
    expect(qr.querySelector("path")?.getAttribute("d")).toMatch(/^M\d+ \d+h1v1h-1z/);

    await user.keyboard("{End}");
    const manual = screen.getByRole("tabpanel");
    expect(within(manual).getByText(ADDRESS)).toBeDefined();
    expect(within(manual).getByText(TOKEN)).toBeDefined();
    expect(within(manual).getByText("Sepolia (chain ID 11155111)")).toBeDefined();
  });

  it("copies the address and the exact amount", async () => {
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, "clipboard", { value: { writeText }, configurable: true });
    await renderCheckout();
    await user.click(screen.getByRole("tab", { name: "Send manually" }));
    await user.click(screen.getByRole("button", { name: "Copy Send to address" }));
    await user.click(screen.getByRole("button", { name: "Copy Exact amount" }));
    expect(writeText.mock.calls).toEqual([[ADDRESS], ["100.502512562814070352"]]);
    expect(screen.getAllByText("Copied")).toHaveLength(2);
  });

  it("follows the payment to credited, hides the payment options, and calls onSuccess once", async () => {
    const onSuccess = vi.fn();
    await renderCheckout({ onSuccess });

    served = quote({ payment_status: "seen", confirmations: 2 });
    await poll();
    expect(screen.getByRole("status").textContent).toBe("Payment received, 2 confirmations");
    expect(screen.queryByRole("tablist")).toBeNull();

    served = quote({ status: "complete", payment_status: "credited" });
    await poll();
    await poll();
    expect(screen.getByRole("status").textContent).toBe("Payment credited: $25.00");
    expect(onSuccess).toHaveBeenCalledTimes(1);
    expect(onSuccess).toHaveBeenCalledWith(served);
  });

  it("calls onChange once per status change, not on every poll", async () => {
    const onChange = vi.fn<(state: CheckoutState) => void>();
    await renderCheckout({ onChange });

    served = quote({ payment_status: "seen", confirmations: 2 });
    await poll();
    served = quote({ payment_status: "seen", confirmations: 3 });
    await poll();
    served = quote({ payment_status: "confirming" });
    await poll();

    const statuses = onChange.mock.calls.map(([state]) => state.status);
    expect(statuses.filter((status) => status !== "loading")).toEqual(["waiting", "seen", "confirming"]);
    expect(onChange).toHaveBeenLastCalledWith(expect.objectContaining({ quote: served, error: null }));
  });

  it("tells the payer not to pay after expiry, and calls onExpire", async () => {
    const onExpire = vi.fn();
    await renderCheckout({ onExpire });
    vi.setSystemTime(quote().expires_at * 1000);
    await poll();
    expect(screen.getByRole("status").textContent).toMatch(/expired. Do not send funds/);
    expect(screen.queryByText(ADDRESS)).toBeNull();
    expect(onExpire).toHaveBeenCalledTimes(1);
  });

  it("applies appearance variables", async () => {
    const { container } = await renderCheckout({
      appearance: {
        theme: "dark",
        variables: {
          colorPrimary: "#cdfa50",
          accessibleColorOnColorPrimary: "#161616",
          borderRadius: "2px",
        },
      },
    });
    const root = container.querySelector<HTMLElement>(".pp-root");
    expect(root?.dataset["theme"]).toBe("dark");
    expect(root?.style.getPropertyValue("--pp-color-primary")).toBe("#cdfa50");
    expect(root?.style.getPropertyValue("--pp-accessible-color-on-color-primary")).toBe("#161616");
    expect(root?.style.getPropertyValue("--pp-border-radius")).toBe("2px");
  });

  it("shows the full transaction hash after a wallet payment, linked to the explorer", async () => {
    const hash = `0x${"ab".repeat(32)}`;
    const provider: EthereumProvider = {
      request: ({ method }) => {
        switch (method) {
          case "eth_requestAccounts":
            return Promise.resolve([ADDRESS]);
          case "eth_chainId":
            return Promise.resolve(`0x${quote().chain_id.toString(16)}`);
          case "eth_sendTransaction":
            return Promise.resolve(hash);
          default:
            return Promise.reject(new Error(`unexpected ${method}`));
        }
      },
    };
    vi.stubGlobal("ethereum", provider);
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    await renderCheckout();
    await user.click(screen.getByRole("button", { name: "Pay with crypto (Browser wallet)" }));
    const link = await screen.findByRole("link", { name: hash });
    expect(link.getAttribute("href")).toBe(`https://sepolia.etherscan.io/tx/${hash}`);
  });

  it("pays with the page's own wallet client instead of discovered wallets", async () => {
    const hash = `0x${"cd".repeat(32)}`;
    const methods: string[] = [];
    const provider: EthereumProvider = {
      request: ({ method }) => {
        methods.push(method);
        switch (method) {
          case "eth_chainId":
            return Promise.resolve(`0x${quote().chain_id.toString(16)}`);
          case "eth_sendTransaction":
            return Promise.resolve(hash);
          default:
            return Promise.reject(new Error(`unexpected ${method}`));
        }
      },
    };
    vi.stubGlobal("ethereum", provider);
    const walletClient = createWalletClient({ account: ADDRESS, transport: custom(provider) });
    const user = userEvent.setup({ advanceTimers: (ms) => vi.advanceTimersByTime(ms) });
    await renderCheckout({ walletClient });
    const panel = screen.getByRole("tabpanel");
    expect(within(panel).getAllByRole("button").map((b) => b.getAttribute("aria-label"))).toEqual([
      "Pay with crypto",
    ]);
    await user.click(within(panel).getByRole("button", { name: "Pay with crypto" }));
    await screen.findByRole("link", { name: hash });
    expect(methods).not.toContain("eth_requestAccounts");
  });
});
