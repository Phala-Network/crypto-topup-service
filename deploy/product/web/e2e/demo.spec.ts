import { expect, test, type Page } from "@playwright/test";
import { createPublicClient, erc20Abi, getAddress, http, parseEther } from "viem";
import { sepolia } from "viem/chains";

declare global {
  interface Window {
    anvilRequest(
      method: string,
      params: unknown,
    ): Promise<{ result: unknown } | { error: { code: number; message: string } }>;
  }
}

function env(name: string): string {
  const value = process.env[name];
  if (value === undefined) {
    throw new Error(`${name} is not set; global setup did not run`);
  }
  return value;
}

const STEPS = [
  "quote_created",
  "transfer_seen",
  "finalized",
  "credited",
  "webhook_received",
  "swept",
] as const;

async function tokenBalance(owner: string): Promise<bigint> {
  const chain = createPublicClient({ chain: sepolia, transport: http(env("ANVIL_URL")) });
  return chain.readContract({
    address: getAddress(env("TOKEN_ADDRESS")),
    abi: erc20Abi,
    functionName: "balanceOf",
    args: [getAddress(owner)],
  });
}

/** An EIP-6963 wallet on Anvil holding the payer account; it starts on mainnet. */
async function installWallet(page: Page) {
  const rpc = env("ANVIL_URL");
  await page.exposeFunction("anvilRequest", async (method: string, params: unknown) => {
    const response = await fetch(rpc, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ jsonrpc: "2.0", id: 1, method, params: params ?? [] }),
    });
    const body = (await response.json()) as {
      result?: unknown;
      error?: { code: number; message: string };
    };
    return body.error === undefined ? { result: body.result } : { error: body.error };
  });
  await page.addInitScript(
    ({ account, chainId }) => {
      let current = 1;
      const known = new Set([1]);
      const fail = (code: number, message: string) => Object.assign(new Error(message), { code });
      const provider = {
        async request({ method, params }: { method: string; params?: unknown }): Promise<unknown> {
          const [first] = (params ?? []) as [{ chainId?: string }?];
          switch (method) {
            case "eth_requestAccounts":
            case "eth_accounts":
              return [account];
            case "eth_chainId":
              return `0x${current.toString(16)}`;
            case "wallet_switchEthereumChain": {
              const id = Number(first?.chainId);
              if (!known.has(id)) {
                throw fail(4902, "Unrecognized chain ID");
              }
              current = id;
              return null;
            }
            case "wallet_addEthereumChain":
              known.add(Number(first?.chainId));
              return null;
          }
          if (current !== chainId) {
            throw fail(4901, "wallet is on another chain");
          }
          const answer = await window.anvilRequest(method, params);
          if ("error" in answer) {
            throw fail(answer.error.code, answer.error.message);
          }
          return answer.result;
        },
        on: () => undefined,
        removeListener: () => undefined,
      };
      const info = {
        uuid: "0b6f1e1e-6f0c-4c43-9d7a-2f0d4b0f7a11",
        name: "Test Wallet",
        icon: "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg'/%3E",
        rdns: "test.wallet",
      };
      const announce = () =>
        window.dispatchEvent(
          new CustomEvent("eip6963:announceProvider", { detail: Object.freeze({ info, provider }) }),
        );
      window.addEventListener("eip6963:requestProvider", announce);
      announce();
    },
    { account: env("PAYER_ADDRESS"), chainId: sepolia.id },
  );
}

test("tops up an account end to end and shows every step behind the scenes", async ({
  page,
  context,
}, testInfo) => {
  await installWallet(page);
  const response = await page.goto(env("DEMO_URL"));
  expect(response?.headers()["content-security-policy"]).toContain("default-src 'none'");

  // Testnet banner, trust strip, and a fresh demo account.
  await expect(page.getByRole("note")).toContainText("Testnet demo.");
  const trust = page.getByRole("region", { name: "Why you can trust Phala Pay" });
  await expect(trust).toContainText("Verified");
  await expect(trust).toContainText("e2e0000000000000000000000000000000000001");
  await expect(page.getByTestId("balance")).toHaveText("$0.00");
  const [cookie] = await context.cookies();
  expect(cookie).toMatchObject({ name: "demo_account", httpOnly: true, sameSite: "Strict" });

  // The payer mints test PHA from the wallet, as the banner offers.
  await page.getByRole("button", { name: "Get 1,000 test PHA" }).click();
  await expect(page.getByRole("note")).toContainText("Minted:");
  expect(await tokenBalance(env("PAYER_ADDRESS"))).toBe(parseEther("1000"));

  // $20 at the fake service's 0.25 USD per PHA: 80 PHA.
  await page.getByText("$20.00", { exact: true }).click();
  await page.getByRole("button", { name: "Pay with crypto", exact: true }).click();
  const timeline = page.getByRole("list", { name: "Payment timeline" });
  await expect(timeline.locator('[data-step="quote_created"]')).toHaveAttribute("data-state", "complete");
  await expect(timeline.locator('[data-step="transfer_seen"]')).toHaveAttribute("data-state", "current");
  await expect(timeline.locator('[data-step="quote_created"]')).toContainText("0.25000000 USD per PHA");
  await page.screenshot({ path: testInfo.outputPath("checkout.png"), fullPage: true });

  await page.getByRole("button", { name: "Pay with crypto (Test Wallet)" }).click();
  await expect(page.getByText(/^Transaction sent:/)).toBeVisible();
  for (const step of STEPS) {
    await expect(timeline.locator(`[data-step="${step}"]`)).toHaveAttribute(
      "data-state",
      "complete",
      { timeout: 60_000 },
    );
  }
  await expect(page.getByRole("status").first()).toHaveText("Payment credited: $20.00");
  await expect(page.getByTestId("balance")).toHaveText("$20.00", { timeout: 10_000 });
  expect(await tokenBalance(env("TREASURY"))).toBe(parseEther("80"));

  // The webhook, as this product received and verified it, and the swept transfer on chain.
  await expect(timeline.locator('[data-step="webhook_received"]')).toContainText("verified");
  await expect(timeline.locator('[data-step="webhook_received"]')).toContainText("+$20.00");
  await expect(page.getByTestId("webhook-event")).toContainText("deposit.credited");
  await expect(page.getByTestId("webhook-event")).toContainText("Verified");
  await expect(timeline.locator('[data-step="swept"] a').first()).toHaveAttribute(
    "href",
    /^https:\/\/sepolia\.etherscan\.io\/tx\/0x[0-9a-f]{64}$/,
  );

  // The history row, with its transaction link.
  const row = page.getByTestId("transaction");
  await expect(row).toContainText("Credited · swept");
  await expect(row).toContainText("80 PHA");
  await expect(row.locator("a")).toHaveAttribute("href", /sepolia\.etherscan\.io\/tx\//);

  // The developer view shows the requests, never the API key or the client secret.
  await page.getByText(/^Developer view/).click();
  const dev = page.locator("details.dev");
  await expect(dev).toContainText("POST /v1/quotes");
  await expect(dev).toContainText("GET /v1/deposits");
  await dev.locator("details.exchange").first().click();
  await expect(dev).toContainText("Bearer ppay_sk_test_…");
  await expect(dev).not.toContainText("AAAAAAAA");
  await expect(dev).toContainText("handed to this browser's checkout");
  await expect(dev).not.toContainText("_secret_5");

  await page.screenshot({ path: testInfo.outputPath("credited-light.png"), fullPage: true });
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.screenshot({ path: testInfo.outputPath("credited-dark.png"), fullPage: true });
  await page.setViewportSize({ width: 420, height: 900 });
  await page.screenshot({ path: testInfo.outputPath("mobile-dark.png"), fullPage: true });
});

test("refuses another browser's quote and rate-limits quote creation", async ({ browser }) => {
  const first = await browser.newContext();
  const page = await first.newPage();
  await page.goto(env("DEMO_URL"));
  await expect(page.getByTestId("balance")).toHaveText("$0.00");
  const created = await page.evaluate(async () => {
    const statuses: number[] = [];
    let quote = "";
    for (let i = 0; i < 4; i += 1) {
      const response = await fetch("api/quotes", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ amount: 500 }),
      });
      statuses.push(response.status);
      const body = (await response.json()) as { quote?: string };
      quote ||= body.quote ?? "";
    }
    return { statuses, quote };
  });
  expect(created.statuses).toEqual([200, 200, 200, 429]);

  const second = await browser.newContext();
  const other = await second.newPage();
  await other.goto(env("DEMO_URL"));
  await expect(other.getByTestId("balance")).toHaveText("$0.00");
  const status = await other.evaluate(
    async (quote) => (await fetch(`api/quotes/${quote}`)).status,
    created.quote,
  );
  expect(status).toBe(404);
  await first.close();
  await second.close();
});
