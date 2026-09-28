import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";
import { defineConfig } from "vite";

// The website, pay.phala.com: one page at `/` with its assets in `assets/`, served by Cloudflare
// (wrangler.jsonc, with the headers of public/_headers). Its demo calls the reference product's API
// at VITE_DEMO_API_ORIGIN (.env.production). Content hashes in file names come from the content
// only, so two builds of the same sources are identical.
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": resolve(import.meta.dirname, "src") } },
  build: { outDir: "dist", emptyOutDir: true, sourcemap: false },
  logLevel: "warn",
});
