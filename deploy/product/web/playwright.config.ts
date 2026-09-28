import { defineConfig, devices } from "@playwright/test";

// Set by sdk/js/e2e/docker.sh: the browser runs in Playwright's image instead of on this host.
const wsEndpoint = process.env["PLAYWRIGHT_WS_ENDPOINT"];

// The global setup starts Anvil, the fake service, and the product's demo API, and builds and
// serves the page from its own origin, which calls the API cross-origin.
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
