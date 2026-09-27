import react from "@vitejs/plugin-react";
import { defineConfig } from "vite";

// Serves the test page, which renders the built package (`dist`), as an integrator's app would.
export default defineConfig({
  root: import.meta.dirname + "/app",
  plugins: [react()],
  logLevel: "warn",
});
