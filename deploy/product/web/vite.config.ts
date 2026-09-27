import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Relative asset URLs: the reference product serves the build under `{public_url}/demo/`
// (reference_product.demo). Content hashes in file names come from the content only, so two builds
// of the same sources are identical (the product image is reproducible).
export default defineConfig({
  base: "./",
  plugins: [react()],
  build: { outDir: "dist", emptyOutDir: true, sourcemap: false },
  logLevel: "warn",
});
