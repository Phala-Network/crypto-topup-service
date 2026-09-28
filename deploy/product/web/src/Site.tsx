import { Braces, CircleDollarSign, Coins, Cpu, FlaskConical, GitBranch, Wallet, type LucideIcon } from "lucide-react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { ThemeToggle, type Theme } from "./theme.js";

const REPO = "https://github.com/Phala-Network/phala-pay";
const LINKS = {
  repo: REPO,
  docs: `${REPO}/blob/main/docs/integration.md`,
  reference: "https://phala-network.github.io/phala-pay/",
  npm: "https://www.npmjs.com/package/@phala/pay",
  license: `${REPO}/blob/main/LICENSE`,
  security: `${REPO}/blob/main/SECURITY.md`,
};

/** The page's width: wide screens get room for the payment and its timeline side by side. */
export const CONTAINER = "mx-auto w-full max-w-[1760px] px-4 sm:px-6 lg:px-8";

// README.md; docs/architecture.md §1 and "Fees and exposure"; docs/integration.md §5.
const PROPERTIES: { icon: LucideIcon; title: string; text: string }[] = [
  {
    icon: Wallet,
    title: "Non-custodial",
    text: "Addresses can only pay your treasury; the service holds no funds and sends no transactions.",
  },
  {
    icon: Coins,
    title: "No fees",
    text: "Phala Pay charges no fee; you pay only the gas of your own sweeps and refunds.",
  },
  {
    icon: Cpu,
    title: "Runs in a TEE",
    text: "A dstack confidential VM, with an attestation you can verify before you trust it.",
  },
  {
    icon: Braces,
    title: "Stripe-shaped API",
    text: "API keys, Idempotency-Key, metadata, Stripe's Event object, and Standard Webhooks.",
  },
];

export function SiteHeader({ theme, onThemeChange }: { theme: Theme; onThemeChange: (theme: Theme) => void }) {
  return (
    <header className="sticky top-0 z-30 border-b bg-background/90 backdrop-blur">
      <div className={`${CONTAINER} flex h-14 items-center justify-between gap-4`}>
        <a href="#top" className="flex items-center gap-2 font-semibold">
          <span className="flex size-7 items-center justify-center rounded-lg bg-primary text-primary-foreground">
            <CircleDollarSign className="size-4" aria-hidden="true" />
          </span>
          Phala Pay
        </a>
        <nav aria-label="Site" className="flex items-center gap-1">
          <Button variant="ghost" asChild>
            <a href={LINKS.repo} aria-label="GitHub">
              <GitBranch aria-hidden="true" />
              <span className="hidden sm:inline">GitHub</span>
            </a>
          </Button>
          <Button variant="ghost" asChild className="hidden sm:inline-flex">
            <a href={LINKS.docs}>Docs</a>
          </Button>
          <Button variant="ghost" asChild className="hidden sm:inline-flex">
            <a href={LINKS.reference}>API reference</a>
          </Button>
          <ThemeToggle theme={theme} onChange={onThemeChange} />
        </nav>
      </div>
    </header>
  );
}

export function Hero() {
  return (
    <section aria-labelledby="hero-title">
      <div className={`${CONTAINER} flex flex-col items-start gap-5 pt-16 pb-12 lg:pt-24 lg:pb-16`}>
        <Badge variant="outline" className="gap-1.5 text-muted-foreground">
          <FlaskConical aria-hidden="true" />
          Sepolia testnet · mainnet not live yet
        </Badge>
        <h1 id="hero-title" className="text-4xl font-semibold tracking-tight sm:text-5xl">
          Phala Pay
        </h1>
        <p className="max-w-2xl text-lg text-muted-foreground sm:text-xl">
          Non-custodial crypto payments in Stripe's shape: your customers pay addresses that can only
          pay your treasury, and a signed webhook tells you what to credit.
        </p>
      </div>
    </section>
  );
}

export function Properties() {
  return (
    <section aria-label="Key properties">
      <ul className={`${CONTAINER} grid gap-x-8 gap-y-6 py-12 sm:grid-cols-2 lg:grid-cols-4 lg:py-16`}>
        {PROPERTIES.map(({ icon: Icon, title, text }) => (
          <li key={title} className="flex flex-col gap-1.5">
            <span className="flex items-center gap-2 text-sm font-medium">
              <Icon className="size-4 text-muted-foreground" aria-hidden="true" />
              {title}
            </span>
            <span className="text-sm text-muted-foreground">{text}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function SiteFooter() {
  const links = [
    { href: LINKS.repo, label: "GitHub" },
    { href: LINKS.docs, label: "Docs" },
    { href: LINKS.reference, label: "API reference" },
    { href: LINKS.npm, label: "npm @phala/pay" },
    { href: LINKS.security, label: "Security" },
  ];
  return (
    <footer className="border-t">
      <div
        className={`${CONTAINER} flex flex-col gap-4 py-8 text-sm text-muted-foreground md:flex-row md:items-center md:justify-between`}
      >
        <p>
          Phala Pay is open source under the{" "}
          <a className="underline underline-offset-4 hover:text-foreground" href={LINKS.license}>
            Apache-2.0 license
          </a>
          .
        </p>
        <nav aria-label="Developers" className="flex flex-wrap items-center gap-x-5 gap-y-2">
          {links.map(({ href, label }) => (
            <a key={label} className="hover:text-foreground" href={href}>
              {label}
            </a>
          ))}
        </nav>
      </div>
    </footer>
  );
}
