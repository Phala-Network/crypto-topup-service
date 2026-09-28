import tailwindcss from "@tailwindcss/vite";
import { tanstackStart } from "@tanstack/react-start/plugin/vite";
import react from "@vitejs/plugin-react";
import { writeFileSync } from "node:fs";
import { join, resolve } from "node:path";
import { defineConfig } from "vite";

const client = resolve(import.meta.dirname, "dist/client");

// The router serializes each match's load time (Date.now()) into the page for its client, which
// only compares it with route loaders' staleTime and gcTime; these routes have no loaders. A fixed
// value keeps two builds identical. Fails the build if the page carries none, so a change in the
// router's format cannot silently make the output differ between builds.
const MATCH_LOAD_TIME = /(\{i:"[^"]*",u:)\d+(?=,)/g;

function reproducible({ page, html }: { page: { path: string }; html: string }): void {
  const fixed = html.replace(MATCH_LOAD_TIME, "$10");
  if (fixed === html) {
    throw new Error(`${page.path}: no match load time to fix in the prerendered page`);
  }
  writeFileSync(join(client, page.path, "index.html"), fixed);
}

// The website the reference product serves (reference_product.demo), a TanStack Start app built
// to static files in `dist/client`: the landing page at `/` (prerendered), the demo at `/demo/`
// (a prerendered document shell; the page renders in the browser), and their shared `assets/`.
// No server runs it in production. Content hashes in file names come from the content only, so
// two builds of the same sources are identical (the product image is reproducible).
export default defineConfig({
  plugins: [
    tanstackStart({
      prerender: {
        enabled: true,
        autoStaticPathsDiscovery: false,
        crawlLinks: false,
        failOnError: true,
        onSuccess: reproducible,
      },
      pages: [{ path: "/" }, { path: "/demo/" }],
    }),
    react(),
    tailwindcss(),
  ],
  resolve: { alias: { "@": resolve(import.meta.dirname, "src") } },
  build: { sourcemap: false },
  // The prerender renders the pages through Vite's preview server, then requests them at the URL
  // it reports: one address, where `localhost` may resolve to IPv6 first (as in Docker builds).
  preview: { host: "127.0.0.1" },
  logLevel: "warn",
});
