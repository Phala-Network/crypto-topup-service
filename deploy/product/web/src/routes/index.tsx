import { createFileRoute } from "@tanstack/react-router";
import { Landing } from "../landing/Landing.js";

export const Route = createFileRoute("/")({
  head: () => ({
    meta: [
      { title: "Phala Pay — non-custodial crypto payments in Stripe's shape" },
      {
        name: "description",
        content:
          "Phala Pay: crypto payments in Stripe's shape. Non-custodial forwarder addresses pay only your treasury; signed webhooks tell you what to credit.",
      },
    ],
  }),
  component: Landing,
});
