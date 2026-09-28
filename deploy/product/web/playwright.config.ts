import { defineConfig, devices } from "@playwright/test";

// Set by sdk/js/e2e/docker.sh: the browser runs in Playwright's image instead of on this host.
const wsEndpoint = process.env["PLAYWRIGHT_WS_ENDPOINT"];

// The product, the fake service, and Anvil are started by the global setup; the page under test
// is the product's own `/demo/`, built into `dist/client` (run `pnpm run build` first).
export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/global-setup.ts",
  forbidOnly: !!process.env["CI"],
  retries: 0,
  workers: 1,
  timeout: 120_000,
  reporter: process.env["CI"] ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    trace: "retain-on-failure",
    ...(wsEndpoint === undefined ? {} : { connectOptions: { wsEndpoint } }),
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"], viewport: { width: 1360, height: 1000 } } }],
});
