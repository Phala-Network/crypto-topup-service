import { createRouter } from "@tanstack/react-router";
import { routeTree } from "./routeTree.gen.js";

export function getRouter() {
  return createRouter({
    routeTree,
    // The product serves the demo at exactly `/demo/`, and its API calls are relative to it.
    trailingSlash: "always",
  });
}
