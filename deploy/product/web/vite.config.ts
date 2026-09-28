import tailwindcss from "@tailwindcss/vite";
import react from "@vitejs/plugin-react";
import { resolve } from "node:path";
import { defineConfig } from "vite";

// The website the reference product serves (reference_product.demo): the landing page at
// `{public_url}/` and the demo at `{public_url}/demo/`, sharing `assets/`. Relative asset URLs,
// so it works under any `public_url` path. Content hashes in file names come from the content
// only, so two builds of the same sources are identical (the product image is reproducible).
export default defineConfig({
  base: "./",
  plugins: [react(), tailwindcss()],
  resolve: { alias: { "@": resolve(import.meta.dirname, "src") } },
  build: {
    outDir: "dist",
    emptyOutDir: true,
    sourcemap: false,
    rolldownOptions: {
      input: { landing: resolve(import.meta.dirname, "index.html"), demo: resolve(import.meta.dirname, "demo/index.html") },
    },
  },
  logLevel: "warn",
});
