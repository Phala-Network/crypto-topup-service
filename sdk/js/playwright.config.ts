import { defineConfig, devices } from "@playwright/test";

const PORT = 5179;
// Set by e2e/docker.sh: the browser runs in Playwright's image instead of on this host.
const wsEndpoint = process.env["PLAYWRIGHT_WS_ENDPOINT"];

export default defineConfig({
  testDir: "e2e",
  globalSetup: "./e2e/global-setup.ts",
  forbidOnly: !!process.env["CI"],
  retries: 0,
  workers: 1,
  reporter: process.env["CI"] ? [["list"], ["html", { open: "never" }]] : "list",
  use: {
    baseURL: `http://127.0.0.1:${PORT}`,
    trace: "retain-on-failure",
    ...(wsEndpoint === undefined ? {} : { connectOptions: { wsEndpoint } }),
  },
  projects: [{ name: "chromium", use: { ...devices["Desktop Chrome"] } }],
  webServer: {
    command: `vite --config e2e/vite.config.ts --port ${PORT} --strictPort --host 127.0.0.1`,
    url: `http://127.0.0.1:${PORT}`,
    reuseExistingServer: !process.env["CI"],
  },
});
