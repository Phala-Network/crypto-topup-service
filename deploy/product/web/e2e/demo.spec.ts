import { expect, test, type Locator, type Page } from "@playwright/test";
import {
  createPublicClient,
  createTestClient,
  encodeFunctionData,
  erc20Abi,
  getAddress,
  http,
  parseAbi,
  parseEther,
  parseUnits,
  publicActions,
  walletActions,
  type Hash,
} from "viem";
import { baseSepolia, sepolia } from "viem/chains";

declare global {
  interface Window {
    anvilRequest(
      chainId: number,
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

/** The owner's balance of a token: Sepolia's test PHA unless named. */
async function tokenBalance(
  owner: string,
  { rpc = env("ANVIL_URL"), token = env("TOKEN_ADDRESS") }: { rpc?: string; token?: string } = {},
): Promise<bigint> {
  const chain = createPublicClient({ transport: http(rpc) });
  return chain.readContract({
    address: getAddress(token),
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

/** The test USDC's public mint, from the test (the page offers Circle's faucet instead). */
async function mintUsdc(to: string, amount: bigint): Promise<void> {
  const chain = createTestClient({ mode: "anvil", chain: sepolia, transport: http(env("ANVIL_URL")) })
    .extend(walletActions)
    .extend(publicActions);
  const payer = getAddress(env("PAYER_ADDRESS"));
  await chain.waitForTransactionReceipt({
    hash: await chain.sendTransaction({
      account: payer,
      to: getAddress(env("USDC_ADDRESS")),
      data: encodeFunctionData({
        abi: parseAbi(["function mint(address account, uint256 amount)"]),
        functionName: "mint",
        args: [getAddress(to), amount],
      }),
    }),
  });
}

/** An EIP-6963 wallet on the Anvils holding the payer account; it starts on mainnet. */
async function installWallet(page: Page) {
  const rpcs: Record<number, string> = { [sepolia.id]: env("ANVIL_URL"), [baseSepolia.id]: env("BASE_ANVIL_URL") };
  await page.exposeFunction("anvilRequest", async (chainId: number, method: string, params: unknown) => {
    const rpc = rpcs[chainId];
    if (rpc === undefined) {
      return { error: { code: 4901, message: "wallet is on another chain" } };
    }
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
    ({ account, chainIds }) => {
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
          if (!chainIds.includes(current)) {
            throw fail(4901, "wallet is on another chain");
          }
          const answer = await window.anvilRequest(current, method, params);
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
    { account: env("PAYER_ADDRESS"), chainIds: [sepolia.id, baseSepolia.id] as number[] },
  );
}

/**
 * The network, then the token, chosen by default: Sepolia, and the first of its two tokens, test
 * PHA, which earns the demo merchant's bonus; test USDC is a stablecoin.
 */
async function expectPaymentOptions(product: Locator) {
  await expect(product.getByRole("combobox", { name: "Network" })).toContainText("Sepolia");
  const token = product.getByRole("radiogroup", { name: "Token" });
  await expect(token.getByRole("radio")).toHaveCount(2);
  await expect(token.getByRole("radio", { name: "Test PHA", exact: true })).toBeChecked();
  const rows = product.getByTestId("token-option");
  await expect(rows.filter({ hasText: "PHA" })).toContainText("+10% bonus");
  // A stablecoin is $1.00; a spot token is at the market rate, which a quote locks.
  await expect(rows.filter({ hasText: "USDC" }).getByTestId("token-price")).toHaveText("$1.00");
  await expect(rows.filter({ hasText: "PHA" }).getByTestId("token-price")).toHaveText("Market rate");
  await expect(rows.filter({ hasText: "USDC" })).not.toContainText("bonus");
}

/** Chooses a network in the product's network select. */
async function chooseNetwork(page: Page, product: Locator, name: string) {
  await product.getByRole("combobox", { name: "Network" }).click();
  await page.getByRole("option", { name: new RegExp(`^${name}`) }).click();
  await expect(product.getByRole("combobox", { name: "Network" })).toContainText(name);
}

/** Collects the page's console errors and CSP violations; the flows expect none. */
async function watchConsole(page: Page): Promise<string[]> {
  const problems: string[] = [];
  page.on("console", (message) => {
    if (message.type() === "error") {
      problems.push(message.text());
    }
  });
  page.on("pageerror", (error) => problems.push(error.message));
  await page.addInitScript(() => {
    document.addEventListener("securitypolicyviolation", (event) => {
      console.error(`CSP violation: ${event.violatedDirective} ${event.blockedURI}`);
    });
  });
  return problems;
}

function step(timeline: Locator, key: string): Locator {
  return timeline.locator(`[data-step="${key}"]`);
}

/** Opens a step's line to its hint, time, and data (closed lines render none). */
async function openStep(timeline: Locator, key: string): Promise<Locator> {
  const line = step(timeline, key);
  const trigger = line.getByRole("button").first();
  if ((await trigger.getAttribute("aria-expanded")) !== "true") {
    await trigger.click();
  }
  await expect(trigger).toHaveAttribute("aria-expanded", "true");
  return line;
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
  const problems = await watchConsole(page);
  await installWallet(page);
  const response = await page.goto(env("SITE_URL"));
  expect(response?.headers()["content-security-policy"]).toContain("default-src 'none'");

  // The headline, the product (marked as a testnet demo) beside its backend (the attestation in
  // the backend's Trust tab), and a fresh demo account.
  await expect(page.getByRole("heading", { level: 1 })).toHaveText(
    "Non-custodial crypto payments with a Stripe-shaped API",
  );
  await expect(page.getByRole("navigation", { name: "Site" }).getByRole("link", { name: "Self-hosting" })).toHaveAttribute(
    "href",
    "https://github.com/Phala-Network/phala-pay/blob/main/docs/self-hosting.md",
  );
  const product = page.getByRole("region", { name: "Cloud Console · Billing" });
  await expect(product.getByTestId("testnet-badge")).toHaveText("Testnet");
  const scenes = page.getByRole("complementary", { name: "Behind the scenes" });
  await expect(scenes.getByRole("list", { name: "The steps of a payment" })).toBeVisible();
  const trust = await openTab(scenes, "Trust");
  await expect(trust).toContainText("Attestation verified");
  await expect(trust).toContainText("Verified");
  await expect(trust).toContainText("e2e0000000000000000000000000000000000001");
  await expect(product.getByTestId("balance")).toHaveText("$0.00");
  const [cookie] = await context.cookies();
  expect(cookie).toMatchObject({ name: "demo_account", path: "/", httpOnly: true, sameSite: "Lax" });

  // The payer mints test PHA from the wallet, as the helper below the product offers.
  const testTokens = page.getByRole("note", { name: "Test tokens" });
  await testTokens.getByRole("button", { name: "Mint 1,000 test PHA" }).click();
  await expect(testTokens).toContainText("Minted:");
  expect(await tokenBalance(env("PAYER_ADDRESS"))).toBe(parseEther("1000"));

  // The customer picks the amount, the network, then the token: the product's one network,
  // Sepolia, and its one token, test PHA, shown and chosen.
  await expectPaymentOptions(product);

  // $20 at the fake service's 0.25 USD per PHA: exactly 80 PHA, with an order id in its metadata.
  // The quote is for the chosen network and token.
  await product.getByText("$20.00", { exact: true }).click();
  const quoteRequest = page.waitForRequest((r) => r.method() === "POST" && r.url() === `${env("API_URL")}/api/quotes`);
  await product.getByRole("button", { name: "Pay with crypto", exact: true }).click();
  expect((await quoteRequest).postDataJSON()).toEqual({ amount: 2000, chain_id: sepolia.id, asset: "pha" });
  // The quote's locked rate; the SDK's status line holds the one countdown.
  const rate = product.getByTestId("locked-rate");
  await expect(rate).toContainText("Locked rate · Test PHA");
  await expect(rate).toContainText("1 PHA = $0.25");
  await expect(product.getByLabel("Time left to pay")).toHaveText(/^1[45]:\d\d$/);
  await expect(product.getByText(/\d+:\d\d$/)).toHaveCount(1);
  const timeline = scenes.getByRole("list", { name: "Payment timeline" });
  await expect(step(timeline, "quote_created")).toHaveAttribute("data-state", "complete");
  await expect(step(timeline, "sent")).toHaveAttribute("data-state", "current");
  const created = await openStep(timeline, "quote_created");
  await expect(created).toContainText("1 PHA = $0.25");
  await expect(created).toContainText("80 PHA");
  const order = (await scenes.getByTestId("meta-order").getAttribute("title"))?.match(/^order_[0-9a-f]{12}$/)?.[0];
  expect(order).toBeDefined();
  // Nothing of the backend shows in the product.
  await expect(product).not.toContainText("order_");
  await page.screenshot({ path: testInfo.outputPath("checkout.png"), fullPage: true });

  await product.getByRole("button", { name: "Pay with crypto (Test Wallet)" }).click();
  await expect(product.getByText(/^Transaction sent:/)).toBeVisible();
  await expectComplete(timeline, ["sent", "received", "credited", "webhook_received"]);
  // One confirmation: the credit, the demo merchant's bonus, the total, and the transaction.
  const confirmation = product.getByTestId("payment-credited");
  await expect(confirmation).toContainText("Payment credited");
  await expect(confirmation).toContainText("$20.00");
  // Nothing pending once credited: the locked rate is gone with the countdown.
  await expect(rate).toHaveCount(0);
  // The demo merchant's +10% PHA bonus, a line of its own: $20.00 and $2.00.
  await expect(product.getByTestId("bonus-credited")).toContainText("+$2.00", { timeout: 10_000 });
  await expect(confirmation).toContainText("Total$22.00");
  await expect(confirmation.getByRole("link")).toHaveAttribute("href", /\/tx\/0x[0-9a-f]{64}$/);
  await expect(product.getByTestId("balance")).toHaveText("$22.00", { timeout: 10_000 });
  // Real times: the block's, then each step's, with the elapsed time since sending.
  const credited = await openStep(timeline, "credited");
  await expect(credited).toContainText("after sending");
  await expect(credited).toContainText("the quote's locked price");
  // Each step opens to its data. The order id arrives in the verified deposit.credited's
  // data.object.metadata.
  await openStep(timeline, "webhook_received");
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
  const sweeps = (await openTab(scenes, "Sweeps")).getByRole("region", { name: "PHA on Sepolia testnet" });
  await expect(sweeps.getByTestId("unswept")).toContainText("80 PHA in 1 forwarder", { timeout: 30_000 });
  await sweeps.getByRole("button", { name: "Sign the flush from my wallet" }).click();
  await expect(sweeps.getByTestId("flush-status")).toContainText("Flush sent: 0x");
  await expectComplete(timeline, ["swept"]);
  expect(await tokenBalance(env("TREASURY"))).toBe(parseEther("80"));
  await expect((await openStep(timeline, "swept")).locator("a").first()).toHaveAttribute(
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
  // The bonus follows the credit down: 10% of $15.00.
  await expect(product.getByTestId("balance")).toHaveText("$16.50", { timeout: 10_000 });
  await expect(scenes.getByTestId("console-net")).toContainText("−$5.00 by deposit.refunded");
  await expect(scenes.getByTestId("console-bonus")).toContainText("+$1.50");

  // A refund paid from another wallet fails verification: the service checks the sender.
  const wrong = await declareRefund(scenes, "20");
  await wrong.getByRole("button", { name: "Pay it from my wallet instead" }).click();
  await expect(wrong.getByLabel("Transaction hash of the payment")).toHaveValue(/^0x[0-9a-f]{64}$/);
  await wrong.getByRole("button", { name: "Mark paid" }).click();
  await expect(wrong).toHaveAttribute("data-status", "failed", { timeout: 60_000 });
  await expect(wrong).toContainText("sender_mismatch");
  await expect(product.getByTestId("balance")).toHaveText("$16.50");

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
  await expect(row).toContainText("at $0.25 / PHA");
  await expect(row).toContainText("$15.00");
  await expect(row).toContainText("+$1.50 bonus");
  // How the balance adds up: the credit and its refund, and the bonus and its claw-back, apart.
  const credits = scenes.locator('[data-testid="ledger-line"][data-kind="credit"]');
  const bonuses = scenes.locator('[data-testid="ledger-line"][data-kind="bonus"]');
  await expect(credits.filter({ hasText: "deposit.credited" })).toContainText("+$20.00");
  await expect(credits.filter({ hasText: "deposit.refunded" })).toContainText("−$5.00");
  await expect(bonuses.filter({ hasText: "PHA bonus +10%" })).toContainText("+$2.00");
  await expect(bonuses.filter({ hasText: "deposit.refunded" })).toContainText("−$0.50");

  await page.screenshot({ path: testInfo.outputPath("refunds-light.png"), fullPage: true });
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.screenshot({ path: testInfo.outputPath("refunds-dark.png"), fullPage: true });
  await page.setViewportSize({ width: 420, height: 900 });
  await page.screenshot({ path: testInfo.outputPath("mobile-dark.png"), fullPage: true });

  // A quote that ends unpaid, expired or canceled, drops its locked rate too.
  await page.setViewportSize({ width: 1360, height: 1000 });
  const followed = scenes.locator('[title^="qt_"]');
  for (const [end, message] of [
    ["expire", "This quote has expired"],
    ["cancel", "This quote was canceled"],
  ] as const) {
    const before = await followed.getAttribute("title");
    await product.getByRole("button", { name: "Start a new top-up" }).click();
    await product.getByText("$5.00", { exact: true }).click();
    await product.getByRole("button", { name: "Pay with crypto", exact: true }).click();
    await expect(rate).toContainText("1 PHA = $0.25");
    await expect(followed).not.toHaveAttribute("title", before ?? "");
    const quote = (await followed.getAttribute("title")) ?? "";
    const ended = await fetch(`${env("SERVICE_URL")}/_test/quotes/${quote}/${end}`, { method: "POST" });
    expect(ended.status).toBe(200);
    await expect(product.getByRole("status").first()).toContainText(message, { timeout: 10_000 });
    await expect(rate).toHaveCount(0);
  }
  expect(problems).toEqual([]);
});

test("a deposit address: one verified address, any amount credited at spot, then reversed", async ({
  page,
}, testInfo) => {
  test.setTimeout(180_000);
  const problems = await watchConsole(page);
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

  await expectPaymentOptions(product);
  await product.getByRole("button", { name: "Show my deposit address" }).click();
  // The backend sees the address checked against the product's pins.
  await expect(scenes.getByTestId("deposit-address-verified")).toContainText("Verified");
  // Its networks and tokens, named as the product's selectors name them.
  // One address on both networks (the treasury is the same), each with its tokens.
  const tokensOnNetwork = scenes.getByTestId("deposit-address-network");
  await expect(tokensOnNetwork).toHaveText(["Test PHA, Test USDC", "Test PHA"]);
  await expect(tokensOnNetwork.first().locator("xpath=..")).toContainText("Sepolia testnet");
  await expect(tokensOnNetwork.last().locator("xpath=..")).toContainText("Base Sepolia testnet");
  const address = (await scenes.getByTestId("deposit-address").textContent()) ?? "";
  expect(address).toMatch(/^0x[0-9a-fA-F]{40}$/);
  // The SDK's <DepositAddress> shows the customer the same address to copy.
  await expect(product.getByText(address).first()).toBeVisible();

  // Any amount, sent from a wallet as from an exchange.
  const testTokens = page.getByRole("note", { name: "Test tokens" });
  await testTokens.getByRole("button", { name: "Mint 1,000 test PHA" }).click();
  await expect(testTokens).toContainText("Minted:");
  const form = product.getByRole("form", { name: "Pay to the deposit address from a browser wallet" });
  await form.getByLabel(/^Send from your browser wallet/).fill("25");
  await form.getByRole("button", { name: "Send" }).click();
  await expect(form).toContainText("Sent: 0x");

  // The backend follows the payment as it arrives.
  const payment = scenes.getByTestId("address-payment").first();
  await expect(payment).toContainText("25 PHA");
  await expect(payment.getByRole("button", { name: /^View/ })).toHaveAttribute("aria-pressed", "true");
  const timeline = scenes.getByRole("list", { name: "Payment timeline" });
  await expectComplete(timeline, ["sent", "received", "credited", "webhook_received"]);

  // Before it is final, the service's finality watch proves the transaction dropped (here, the
  // stand-in's test hook): deposit.reversed takes the credit back.
  const deposit = await page.evaluate(async (api) => {
    const response = await fetch(`${api}/api/deposit_address`, { credentials: "include" });
    return ((await response.json()) as { deposit_address: { payments: { deposit: string }[] } }).deposit_address
      .payments[0]?.deposit;
  }, env("API_URL"));
  const reversed = await fetch(`${env("SERVICE_URL")}/_test/deposits/${deposit ?? ""}/reverse`, { method: "POST" });
  expect(reversed.status).toBe(200);

  // 25 PHA at 0.25 USD, credited at spot; the address's metadata arrived with the deposit.
  await openStep(timeline, "credited");
  await openStep(timeline, "webhook_received");
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
  // The customer sees each payment at the rate it was credited at.
  await expect(product.getByTestId("top-up").first()).toContainText("25 Test PHA");
  await expect(product.getByTestId("top-up").first()).toContainText("Credited at $0.25 / PHA, then reversed");
  await openTab(scenes, "Payments");
  const lines = scenes.getByTestId("ledger-line");
  await expect(lines.filter({ hasText: "deposit.credited" })).toContainText("+$6.25");
  // A reversal takes the whole bonus back with the credit.
  await expect(lines.filter({ hasText: "PHA bonus +10%" })).toContainText("+$0.62");
  const reversals = lines.filter({ hasText: "deposit.reversed" });
  await expect(reversals.and(scenes.locator('[data-kind="credit"]'))).toContainText("−$6.25");
  await expect(reversals.and(scenes.locator('[data-kind="bonus"]'))).toContainText("−$0.62");
  await page.screenshot({ path: testInfo.outputPath("deposit-address.png"), fullPage: true });
  expect(problems).toEqual([]);
});

test("networks and tokens: USDC at $1.00 without a bonus, and PHA on Base Sepolia with one; the faucets follow", async ({
  page,
}, testInfo) => {
  test.setTimeout(180_000);
  const problems = await watchConsole(page);
  await installWallet(page);
  await page.goto(env("SITE_URL"));
  const product = page.getByRole("region", { name: "Cloud Console · Billing" });
  const scenes = page.getByRole("complementary", { name: "Behind the scenes" });
  const helper = page.getByRole("note", { name: "Test tokens" });
  await expect(product.getByTestId("balance")).toHaveText("$0.00");
  await expectPaymentOptions(product);

  // Test PHA mints from the wallet; gas comes from ethereum.org's list of Sepolia faucets.
  await expect(helper.getByRole("button", { name: "Mint 1,000 test PHA" })).toBeVisible();
  await expect(helper.getByRole("link", { name: /^Sepolia ETH faucets/ })).toHaveAttribute(
    "href",
    "https://ethereum.org/en/developers/docs/networks/#sepolia",
  );

  // Test USDC: Circle's faucet, on the network chosen there; no bonus, at $1.00.
  await product.getByRole("radio", { name: "Test USDC", exact: true }).check({ force: true });
  await expect(helper.getByRole("button", { name: /^Mint/ })).toHaveCount(0);
  await expect(helper.getByRole("link", { name: /^Get test USDC from Circle/ })).toHaveAttribute(
    "href",
    "https://faucet.circle.com",
  );
  await expect(helper.getByTestId("faucet-hint")).toHaveText("On the faucet, pick Sepolia as the network.");
  await mintUsdc(env("PAYER_ADDRESS"), parseUnits("100", 6));
  await product.getByText("$5.00", { exact: true }).click();
  const usdcRequest = page.waitForRequest((r) => r.method() === "POST" && r.url() === `${env("API_URL")}/api/quotes`);
  await product.getByRole("button", { name: "Pay with crypto", exact: true }).click();
  expect((await usdcRequest).postDataJSON()).toEqual({ amount: 500, chain_id: sepolia.id, asset: "usdc" });
  await expect(product.getByTestId("locked-rate")).toContainText("1 USDC = $1.00");
  await expect(product.getByText(/bonus/)).toHaveCount(0);
  await page.setViewportSize({ width: 1440, height: 1100 });
  await page.screenshot({ path: testInfo.outputPath("usdc-quote.png") });
  await product.getByRole("button", { name: "Pay with crypto (Test Wallet)" }).click();
  await expect(product.getByTestId("payment-credited")).toContainText("$5.00", { timeout: 60_000 });
  await expect(product.getByTestId("balance")).toHaveText("$5.00", { timeout: 10_000 });
  await expect(product.getByTestId("bonus-credited")).toHaveCount(0);
  expect(await tokenBalance(env("PAYER_ADDRESS"), { token: env("USDC_ADDRESS") })).toBe(parseUnits("95", 6));
  await openTab(scenes, "Payments");
  await expect(scenes.getByTestId("payment").first()).toContainText("5 USDC");
  await expect(scenes.getByTestId("payment").first()).toContainText("at $1.00 / USDC");

  // Base Sepolia: its own token list and faucets; test PHA mints there, from the wallet on that
  // network, and a PHA quote there earns the bonus, at staging's rate, formatted.
  await product.getByRole("button", { name: "Start a new top-up" }).click();
  await chooseNetwork(page, product, "Base Sepolia");
  const tokens = product.getByRole("radiogroup", { name: "Token" });
  await expect(tokens.getByRole("radio")).toHaveCount(1);
  await expect(tokens.getByRole("radio", { name: "Test PHA", exact: true })).toBeChecked();
  await expect(helper.getByRole("link", { name: /^Base Sepolia ETH faucets/ })).toHaveAttribute(
    "href",
    "https://docs.base.org/get-started/get-funds#testnet-base-sepolia",
  );
  await helper.getByRole("button", { name: "Mint 1,000 test PHA" }).click();
  await expect(helper).toContainText("Minted:");
  const base = { rpc: env("BASE_ANVIL_URL"), token: env("BASE_TOKEN_ADDRESS") };
  expect(await tokenBalance(env("PAYER_ADDRESS"), base)).toBe(parseEther("1000"));
  await expect(helper.locator("p", { hasText: "Minted:" }).getByRole("link")).toHaveAttribute(
    "href",
    /^https:\/\/sepolia\.basescan\.org\/tx\//,
  );
  const baseRequest = page.waitForRequest((r) => r.method() === "POST" && r.url() === `${env("API_URL")}/api/quotes`);
  await product.getByRole("button", { name: "Pay with crypto", exact: true }).click();
  expect((await baseRequest).postDataJSON()).toEqual({ amount: 2000, chain_id: baseSepolia.id, asset: "pha" });
  await expect(product.getByTestId("locked-rate")).toContainText("1 PHA = $0.06041");
  await expect(product.getByTestId("testnet-badge")).toBeVisible();
  await product.getByRole("button", { name: "Pay with crypto (Test Wallet)" }).click();
  await expect(product.getByTestId("payment-credited")).toContainText("$20.00", { timeout: 60_000 });
  await expect(product.getByTestId("bonus-credited")).toContainText("+$2.00", { timeout: 10_000 });
  await expect(product.getByTestId("balance")).toHaveText("$27.00", { timeout: 10_000 });
  await page.getByRole("button", { name: "Switch to dark theme" }).click();
  await page.screenshot({ path: testInfo.outputPath("base-bonus-dark.png") });
  await page.getByRole("button", { name: "Switch to light theme" }).click();
  expect(problems).toEqual([]);
});

test("refuses another browser's payments and refunds, and rate-limits quote creation", async ({ browser }) => {
  const first = await browser.newContext();
  const page = await first.newPage();
  await page.goto(env("SITE_URL"));
  await expect(page.getByTestId("balance")).toHaveText("$0.00");
  const created = await page.evaluate(async (api) => {
    const statuses: number[] = [];
    let quote = "";
    for (let i = 0; i < 4; i += 1) {
      const response = await fetch(`${api}/api/quotes`, {
        method: "POST",
        credentials: "include",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ amount: 500, chain_id: 11155111, asset: "pha" }),
      });
      statuses.push(response.status);
      const body = (await response.json()) as { quote?: string };
      quote ||= body.quote ?? "";
    }
    return { statuses, quote };
  }, env("API_URL"));
  expect(created.statuses).toEqual([200, 200, 200, 429]);
  // The API's origin serves only the API: no page, and the old `/demo/` paths are unknown.
  for (const path of ["", "index.html", "demo", "demo/", "demo/api/account"]) {
    expect((await fetch(`${env("API_URL")}/${path}`)).status).toBe(404);
  }
  // Another origin's page cannot read the API, even with the visitor's cookie.
  const elsewhere = await first.newPage();
  await elsewhere.goto(`${env("SERVICE_URL")}/evidences/quote.json`);
  const refused = await elsewhere.evaluate(async (api) => {
    try {
      await fetch(`${api}/api/account`, { credentials: "include" });
      return "read";
    } catch {
      return "refused";
    }
  }, env("API_URL"));
  expect(refused).toBe("refused");

  const second = await browser.newContext();
  const other = await second.newPage();
  await other.goto(env("SITE_URL"));
  await expect(other.getByTestId("balance")).toHaveText("$0.00");
  const statuses = await other.evaluate(
    async ({ api, quote }) => {
      const get = (path: string) => fetch(`${api}/api/${path}`, { credentials: "include" });
      const post = (path: string, body: unknown) =>
        fetch(`${api}/api/${path}`, {
          method: "POST",
          credentials: "include",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(body),
        });
      return [
        (await get(`quotes/${quote}`)).status,
        (await get(`deposits/dep_${"0".repeat(32)}`)).status,
        (await post(`refunds/re_${"0".repeat(32)}/cancel`, {})).status,
        (await post(`refunds/re_${"0".repeat(32)}/mark_paid`, { transaction_hash: `0x${"ab".repeat(32)}` })).status,
      ];
    },
    { api: env("API_URL"), quote: created.quote },
  );
  expect(statuses).toEqual([404, 404, 404, 404]);
  await first.close();
  await second.close();
});
