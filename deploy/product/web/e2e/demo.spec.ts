import { expect, test, type Locator, type Page } from "@playwright/test";
import {
  createPublicClient,
  createTestClient,
  encodeFunctionData,
  erc20Abi,
  getAddress,
  http,
  parseEther,
  walletActions,
  type Hash,
} from "viem";
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

async function tokenBalance(owner: string): Promise<bigint> {
  const chain = createPublicClient({ chain: sepolia, transport: http(env("ANVIL_URL")) });
  return chain.readContract({
    address: getAddress(env("TOKEN_ADDRESS")),
    abi: erc20Abi,
    functionName: "balanceOf",
    args: [getAddress(owner)],
  });
}

/**
 * Pays `amount` of the test token from the treasury: on Anvil the test plays the merchant's
 * finance team, which controls the treasury (on staging it is Phala's finance Safe).
 */
async function payFromTreasury(to: string, amount: bigint): Promise<Hash> {
  const treasury = getAddress(env("TREASURY"));
  const chain = createTestClient({ mode: "anvil", chain: sepolia, transport: http(env("ANVIL_URL")) }).extend(
    walletActions,
  );
  await chain.impersonateAccount({ address: treasury });
  await chain.setBalance({ address: treasury, value: parseEther("1") });
  const hash = await chain.sendTransaction({
    account: treasury,
    to: getAddress(env("TOKEN_ADDRESS")),
    data: encodeFunctionData({ abi: erc20Abi, functionName: "transfer", args: [getAddress(to), amount] }),
  });
  await chain.stopImpersonatingAccount({ address: treasury });
  return hash;
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

function step(timeline: Locator, key: string): Locator {
  return timeline.locator(`[data-step="${key}"]`);
}

async function expectComplete(timeline: Locator, keys: string[], timeout = 60_000) {
  for (const key of keys) {
    await expect(step(timeline, key)).toHaveAttribute("data-state", "complete", { timeout });
  }
}

/** Opens one of the backend's tabs and returns its panel. */
async function openTab(scenes: Locator, name: string): Promise<Locator> {
  await scenes.getByRole("tab", { name: new RegExp(`^${name}`) }).click();
  return scenes.getByRole("tabpanel", { name: new RegExp(`^${name}`) });
}

/** Declares a refund of `amount` PHA of the selected deposit and returns its list item. */
async function declareRefund(scenes: Locator, amount: string): Promise<Locator> {
  await openTab(scenes, "Refunds");
  const refunds = scenes.getByTestId("refund");
  const before = await refunds.count();
  const form = scenes.getByRole("form", { name: "Declare a refund" });
  await form.getByLabel(/^Amount/).fill(amount);
  await form.getByRole("button", { name: "Declare refund" }).click();
  await expect(refunds).toHaveCount(before + 1);
  // Newest first; followed by its id from here on.
  const id = await refunds.first().getAttribute("data-refund");
  const refund = scenes.locator(`[data-refund="${id ?? ""}"]`);
  await expect(refund).toHaveAttribute("data-status", "pending");
  return refund;
}

test("a quote: locked price, metadata, the merchant's sweep, and refunds that succeed, fail, or are canceled", async ({
  page,
  context,
}, testInfo) => {
  test.setTimeout(300_000);
  await installWallet(page);
  const response = await page.goto(env("SITE_URL"));
  expect(response?.headers()["content-security-policy"]).toContain("default-src 'none'");

  // The headline, the testnet notice, the product beside its backend (the attestation in the
  // backend's Trust tab), and a fresh demo account.
  await expect(page.getByRole("heading", { level: 1 })).toHaveText("Crypto payments, without custody.");
  await expect(page.getByText("Sepolia testnet · mainnet not live yet")).toBeVisible();
  const product = page.getByRole("region", { name: "Cloud Console · Billing" });
  const scenes = page.getByRole("complementary", { name: "Behind the scenes" });
  await expect(scenes.getByRole("list", { name: "The steps of a payment" })).toBeVisible();
  const trust = await openTab(scenes, "Trust");
  await expect(trust).toContainText("Attestation verified");
  await expect(trust).toContainText("Verified");
  await expect(trust).toContainText("e2e0000000000000000000000000000000000001");
  await expect(product.getByTestId("balance")).toHaveText("$0.00");
  const [cookie] = await context.cookies();
  expect(cookie).toMatchObject({ name: "demo_account", httpOnly: true, sameSite: "Strict" });

  // The payer mints test PHA from the wallet, as the testnet notice offers.
  const testnet = page.getByRole("note", { name: "Testnet demo" });
  await testnet.getByRole("button", { name: "Get 1,000 test PHA" }).click();
  await expect(testnet).toContainText("Minted:");
  expect(await tokenBalance(env("PAYER_ADDRESS"))).toBe(parseEther("1000"));

  // $20 at the fake service's 0.25 USD per PHA: exactly 80 PHA, with an order id in its metadata.
  await product.getByText("$20.00", { exact: true }).click();
  await product.getByRole("button", { name: "Pay with crypto", exact: true }).click();
  const timeline = scenes.getByRole("list", { name: "Payment timeline" });
  await expect(step(timeline, "quote_created")).toHaveAttribute("data-state", "complete");
  await expect(step(timeline, "sent")).toHaveAttribute("data-state", "current");
  await expect(step(timeline, "quote_created")).toContainText("0.25000000 USD per PHA");
  await expect(step(timeline, "quote_created")).toContainText("80 PHA");
  const order = (await scenes.getByText(/^Order order_[0-9a-f]{12}/).textContent())?.match(/order_[0-9a-f]{12}/)?.[0];
  expect(order).toBeDefined();
  // Nothing of the backend shows in the product.
  await expect(product).not.toContainText("order_");
  await page.screenshot({ path: testInfo.outputPath("checkout.png"), fullPage: true });

  await product.getByRole("button", { name: "Pay with crypto (Test Wallet)" }).click();
  await expect(product.getByText(/^Transaction sent:/)).toBeVisible();
  await expectComplete(timeline, ["sent", "received", "credited", "webhook_received"]);
  await expect(product.getByRole("status").first()).toHaveText("Payment credited: $20.00");
  await expect(product.getByTestId("balance")).toHaveText("$20.00", { timeout: 10_000 });
  // Real times: the block's, then each step's, with the elapsed time since sending.
  await expect(step(timeline, "credited")).toContainText("after sending");
  await expect(step(timeline, "credited")).toContainText("the quote's locked price");
  // Each step opens to its data. The order id arrives in the verified deposit.credited's
  // data.object.metadata.
  await step(timeline, "webhook_received").getByRole("button").first().click();
  await expect(step(timeline, "webhook_received").getByText("data.object.metadata")).toBeVisible();
  await expect(step(timeline, "webhook_received")).toContainText("verified");
  await expect(step(timeline, "webhook_received")).toContainText(`"order_id": "${order ?? ""}"`);
  await expect(step(timeline, "webhook_received")).toContainText("+$20.00");
  await openTab(scenes, "API");
  await expect(scenes.getByTestId("webhook-event").first()).toContainText("deposit.credited");
  // Refunds wait for finality.
  await openTab(scenes, "Refunds");
  await expect(scenes.getByTestId("refund-unavailable")).toContainText("deposit_not_final");
  await expectComplete(timeline, ["final"]);
  await expect(step(timeline, "swept")).toHaveAttribute("data-state", "current");

  // The merchant sweeps: the SDK's flush, signed from a wallet (anyone may send it; the funds can
  // only reach the treasury), indexed by the service once final.
  const sweeps = await openTab(scenes, "Sweeps");
  await expect(sweeps.getByTestId("unswept")).toContainText("80 PHA in 1 forwarder", { timeout: 30_000 });
  await sweeps.getByRole("button", { name: "Sign the flush from my wallet" }).click();
  await expect(sweeps.getByTestId("flush-status")).toContainText("Flush sent: 0x");
  await expectComplete(timeline, ["swept"]);
  expect(await tokenBalance(env("TREASURY"))).toBe(parseEther("80"));
  await expect(step(timeline, "swept").locator("a").first()).toHaveAttribute(
    "href",
    /^https:\/\/sepolia\.etherscan\.io\/tx\/0x[0-9a-f]{64}$/,
  );
  await expect(sweeps.getByTestId("sweep")).toContainText("80 PHA", { timeout: 30_000 });

  // A refund paid from the treasury succeeds: 20 of 80 PHA takes back a quarter of the credit.
  const paid = await declareRefund(scenes, "20");
  await expect(paid.getByTestId("refund-transfer")).toContainText(env("TREASURY").toLowerCase());
  const hash = await payFromTreasury(env("PAYER_ADDRESS"), parseEther("20"));
  await paid.getByLabel("Transaction hash of the payment").fill(hash);
  await paid.getByRole("button", { name: "Mark paid" }).click();
  await expect(paid).toContainText("Marked paid");
  await expect(paid).toHaveAttribute("data-status", "succeeded", { timeout: 60_000 });
  await expect(paid).toContainText("deposit.refunded");
  await expect(scenes.getByTestId("nets-to")).toHaveText("$15.00");
  await expect(product.getByTestId("balance")).toHaveText("$15.00", { timeout: 10_000 });
  await expect(scenes.getByTestId("console-net")).toContainText("−$5.00 by deposit.refunded");

  // A refund paid from another wallet fails verification: the service checks the sender.
  const wrong = await declareRefund(scenes, "20");
  await wrong.getByRole("button", { name: "Pay it from my wallet instead" }).click();
  await expect(wrong.getByLabel("Transaction hash of the payment")).toHaveValue(/^0x[0-9a-f]{64}$/);
  await wrong.getByRole("button", { name: "Mark paid" }).click();
  await expect(wrong).toHaveAttribute("data-status", "failed", { timeout: 60_000 });
  await expect(wrong).toContainText("sender_mismatch");
  await expect(product.getByTestId("balance")).toHaveText("$15.00");

  // A declared refund without a payment can be canceled.
  const canceled = await declareRefund(scenes, "20");
  await canceled.getByRole("button", { name: "Cancel refund" }).click();
  await expect(canceled).toHaveAttribute("data-status", "canceled");
  await openTab(scenes, "API");
  const events = scenes.getByTestId("webhook-event");
  for (const type of ["refund.created", "refund.updated", "refund.failed", "deposit.refunded"]) {
    await expect(events.filter({ hasText: type }).first()).toBeVisible();
  }

  // The developer view shows the requests, never the API key or a client secret.
  const api = scenes.getByRole("region", { name: /^API requests/ });
  await expect(api).toContainText("GET /v1/deposits");
  await expect(api).toContainText("GET /v1/refunds");
  await api.locator("details").first().click();
  await expect(api).toContainText("Bearer ppay_rk_test_…");
  await expect(api).not.toContainText("AAAAAAAA");

  // The history row and the ledger lines behind the balance.
  await openTab(scenes, "Payments");
  const row = scenes.getByTestId("payment").first();
  await expect(row).toContainText("Quote");
  await expect(row).toContainText("80 PHA");
  await expect(row).toContainText("$15.00");
  await expect(scenes.getByTestId("ledger-line").filter({ hasText: "deposit.refunded" })).toContainText("−$5.00");

  await page.screenshot({ path: testInfo.outputPath("refunds-light.png"), fullPage: true });
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.screenshot({ path: testInfo.outputPath("refunds-dark.png"), fullPage: true });
  await page.setViewportSize({ width: 420, height: 900 });
  await page.screenshot({ path: testInfo.outputPath("mobile-dark.png"), fullPage: true });
});

test("a deposit address: one verified address, any amount credited at spot, then reversed", async ({
  page,
}, testInfo) => {
  test.setTimeout(180_000);
  await installWallet(page);
  await page.goto(env("SITE_URL"));
  const product = page.getByRole("region", { name: "Cloud Console · Billing" });
  const scenes = page.getByRole("complementary", { name: "Behind the scenes" });
  await expect(product.getByTestId("balance")).toHaveText("$0.00");

  // The tabs follow the keyboard.
  await page.getByRole("tab", { name: "Exact amount" }).focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("tab", { name: "Deposit address" })).toHaveAttribute("aria-selected", "true");
  await expect(page.getByRole("tab", { name: "Deposit address" })).toBeFocused();

  await product.getByRole("button", { name: "Show my deposit address" }).click();
  // The backend sees the address checked against the product's pins.
  await expect(scenes.getByTestId("deposit-address-verified")).toContainText("Verified");
  const address = (await scenes.getByTestId("deposit-address").textContent()) ?? "";
  expect(address).toMatch(/^0x[0-9a-fA-F]{40}$/);
  // The SDK's <DepositAddress> shows the customer the same address to copy.
  await expect(product.getByText(address).first()).toBeVisible();

  // Any amount, sent from a wallet as from an exchange.
  const testnet = page.getByRole("note", { name: "Testnet demo" });
  await testnet.getByRole("button", { name: "Get 1,000 test PHA" }).click();
  await expect(testnet).toContainText("Minted:");
  const form = product.getByRole("form", { name: "Pay to the deposit address from a browser wallet" });
  await form.getByLabel(/^Send from your browser wallet/).fill("25");
  await form.getByRole("button", { name: "Send" }).click();
  await expect(form).toContainText("Sent: 0x");

  // The backend follows the payment as it arrives.
  const payment = scenes.getByTestId("address-payment").first();
  await expect(payment).toContainText("25 PHA");
  await expect(payment.getByRole("button")).toHaveText("Following");
  const timeline = scenes.getByRole("list", { name: "Payment timeline" });
  await expectComplete(timeline, ["sent", "received", "credited", "webhook_received"]);

  // Before it is final, the service's finality watch proves the transaction dropped (here, the
  // stand-in's test hook): deposit.reversed takes the credit back.
  const deposit = await page.evaluate(async () => {
    const response = await fetch("api/deposit_address");
    return ((await response.json()) as { deposit_address: { payments: { deposit: string }[] } }).deposit_address
      .payments[0]?.deposit;
  });
  const reversed = await fetch(`${env("SERVICE_URL")}/_test/deposits/${deposit ?? ""}/reverse`, { method: "POST" });
  expect(reversed.status).toBe(200);

  // 25 PHA at 0.25 USD, credited at spot; the address's metadata arrived with the deposit.
  await expect(step(timeline, "credited")).toContainText("spot");
  await expect(step(timeline, "credited")).toContainText("$6.25");
  await expect(step(timeline, "webhook_received")).toContainText('"workspace": "demo-');
  await expect(step(timeline, "webhook_received")).toContainText("+$6.25");
  await expect(step(timeline, "reversed")).toHaveAttribute("data-state", "failed", { timeout: 30_000 });
  await expect(product.getByTestId("balance")).toHaveText("$0.00", { timeout: 10_000 });
  await openTab(scenes, "Refunds");
  await expect(scenes.getByTestId("nets-to")).toHaveText("$0.00");
  await expect(scenes.getByTestId("refund-unavailable")).toContainText("reversed");
  await openTab(scenes, "API");
  await expect(scenes.getByTestId("webhook-event").filter({ hasText: "deposit.reversed" })).toBeVisible();
  await expect(product.locator(".pp-payments")).toContainText("25 PHA");
  await openTab(scenes, "Payments");
  const lines = scenes.getByTestId("ledger-line");
  await expect(lines.filter({ hasText: "deposit.credited" })).toContainText("+$6.25");
  await expect(lines.filter({ hasText: "deposit.reversed" })).toContainText("−$6.25");
  await page.screenshot({ path: testInfo.outputPath("deposit-address.png"), fullPage: true });
});

test("refuses another browser's payments and refunds, and rate-limits quote creation", async ({ browser }) => {
  const first = await browser.newContext();
  const page = await first.newPage();
  await page.goto(env("SITE_URL"));
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
  // The demo is on the page and its API at `/api/`: the old `/demo/` paths are unknown.
  for (const path of ["demo", "demo/", "demo/api/account"]) {
    expect((await fetch(`${env("SITE_URL")}${path}`)).status).toBe(404);
  }

  const second = await browser.newContext();
  const other = await second.newPage();
  await other.goto(env("SITE_URL"));
  await expect(other.getByTestId("balance")).toHaveText("$0.00");
  const statuses = await other.evaluate(async (quote) => {
    const post = (path: string, body: unknown) =>
      fetch(path, { method: "POST", headers: { "content-type": "application/json" }, body: JSON.stringify(body) });
    return [
      (await fetch(`api/quotes/${quote}`)).status,
      (await fetch(`api/deposits/dep_${"0".repeat(32)}`)).status,
      (await post(`api/refunds/re_${"0".repeat(32)}/cancel`, {})).status,
      (await post(`api/refunds/re_${"0".repeat(32)}/mark_paid`, { transaction_hash: `0x${"ab".repeat(32)}` })).status,
    ];
  }, created.quote);
  expect(statuses).toEqual([404, 404, 404, 404]);
  await first.close();
  await second.close();
});
