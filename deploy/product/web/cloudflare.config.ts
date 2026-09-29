import { defineConfig } from "cf/config";

// pay.phala.com: the page built from this directory, served by a Worker with static assets only (no
// Worker script). The Cloudflare Vite plugin (vite.config.ts) builds the page's assets into cf's
// Build Output. Cloudflare Workers Builds deploys it from this repository (deploy/phala.md,
// "Website").
export default defineConfig({
  worker: {
    name: "phala-pay-web",
    compatibilityDate: "2026-09-26",
    assets: {
      // One page at `/` and its assets, with no client-side routes: any other path is a real 404,
      // not the page (which "single-page-application" would serve).
      notFoundHandling: "none",
    },
    domains: ["pay.phala.com"],
    // The site is served only on its custom domain: no second copy at *.workers.dev.
    workersDev: false,
    // A branch build's uploaded version keeps a public preview URL on workers.dev, to review the
    // page before it ships. Its demo API calls fail by design: the API allows only
    // https://pay.phala.com.
    previewUrls: true,
  },
});
