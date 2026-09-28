import { createFileRoute } from "@tanstack/react-router";
import { App } from "../../App.js";

// The demo reads the visitor's account from the product's API, so it renders in the browser only:
// the prerendered page is the document shell, without the route's content.
export const Route = createFileRoute("/demo/")({
  ssr: false,
  head: () => ({ meta: [{ title: "Billing · Add credits — Phala Pay demo" }] }),
  component: App,
});
