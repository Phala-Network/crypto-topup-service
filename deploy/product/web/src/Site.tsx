import { Braces, Cpu, GitBranch, Server, Wallet, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { ThemeToggle, type Theme } from "./theme.js";

const REPO = "https://github.com/Phala-Network/phala-pay";
const LINKS = {
  repo: REPO,
  docs: `${REPO}/blob/main/docs/integration.md`,
  selfHosting: `${REPO}/blob/main/docs/self-hosting.md`,
  reference: "https://phala-network.github.io/phala-pay/",
  npm: "https://www.npmjs.com/package/@phala/pay",
  license: `${REPO}/blob/main/LICENSE`,
  security: `${REPO}/blob/main/SECURITY.md`,
};

/** The page's width. */
export const CONTAINER = "mx-auto w-full max-w-[84rem] px-5 sm:px-8 2xl:max-w-[92rem]";

// README.md; docs/self-hosting.md; docs/architecture.md §1; docs/integration.md §5.
const PROPERTIES: { icon: LucideIcon; title: string; text: string }[] = [
  {
    icon: Wallet,
    title: "Non-custodial",
    text: "Addresses can only pay your treasury; the service holds no funds and sends no transactions.",
  },
  {
    icon: Server,
    title: "Self-hosted",
    text: "Open source under Apache-2.0: you run your own instance, for your own merchants.",
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
    <header className="sticky top-0 z-30 border-b border-transparent bg-background/80 backdrop-blur-md supports-[backdrop-filter]:bg-background/70">
      <div className={`${CONTAINER} flex h-14 items-center justify-between gap-4`}>
        <a href="#top" className="flex items-center gap-2 rounded-md text-[0.9375rem] font-semibold tracking-tight">
          <Logo />
          Phala Pay
        </a>
        <nav aria-label="Site" className="flex items-center gap-0.5 text-muted-foreground">
          <Button variant="ghost" asChild className="hidden hover:text-foreground md:inline-flex">
            <a href={LINKS.selfHosting}>Self-hosting</a>
          </Button>
          <Button variant="ghost" asChild className="hidden hover:text-foreground sm:inline-flex">
            <a href={LINKS.docs}>Docs</a>
          </Button>
          <Button variant="ghost" asChild className="hidden hover:text-foreground sm:inline-flex">
            <a href={LINKS.reference}>API reference</a>
          </Button>
          <Button variant="ghost" asChild className="hover:text-foreground">
            <a href={LINKS.repo} aria-label="GitHub">
              <GitBranch aria-hidden="true" />
              <span className="hidden sm:inline">GitHub</span>
            </a>
          </Button>
          <ThemeToggle theme={theme} onChange={onThemeChange} />
        </nav>
      </div>
    </header>
  );
}

function Logo() {
  return (
    <span
      className="flex size-6 items-center justify-center rounded-md bg-neutral-950 ring-1 ring-white/15 ring-inset"
      aria-hidden="true"
    >
      <span className="size-2.5 rounded-[3px] bg-brand" />
    </span>
  );
}

// README.md and docs/self-hosting.md: open source, run by each operator, and non-custodial. Phala
// runs an instance only for Phala Cloud and offers no hosted service.
export function Hero() {
  return (
    <section aria-labelledby="hero-title">
      <div className={`${CONTAINER} flex flex-col gap-6 pt-12 pb-10 sm:pt-16 lg:flex-row lg:items-end lg:justify-between lg:gap-16 lg:pt-20 lg:pb-14`}>
        <h1
          id="hero-title"
          className="text-[2.75rem] leading-[1.02] font-semibold tracking-[-0.04em] text-balance sm:text-6xl xl:text-7xl"
        >
          Crypto payments,{" "}
          <br />
          <span className="text-muted-foreground">self-hosted.</span>
        </h1>
        <div className="flex max-w-md flex-col gap-6 lg:pb-2">
          <p className="text-lg leading-relaxed text-pretty text-muted-foreground">
            Open source and non-custodial: run it yourself, and your customers pay addresses that can only pay your
            treasury.
          </p>
          <div className="flex flex-wrap gap-3">
            <Button asChild size="lg" className="h-10 gap-2 rounded-full px-5">
              <a href={LINKS.selfHosting}>
                <Server aria-hidden="true" />
                Self-host it
              </a>
            </Button>
            <Button asChild size="lg" variant="outline" className="h-10 gap-2 rounded-full px-5">
              <a href={LINKS.repo}>
                <GitBranch aria-hidden="true" />
                GitHub
              </a>
            </Button>
          </div>
        </div>
      </div>
    </section>
  );
}

export function Properties() {
  return (
    <section aria-label="Key properties" className={CONTAINER}>
      <ul className="grid gap-x-10 gap-y-8 border-t py-14 sm:grid-cols-2 lg:grid-cols-4 lg:py-20">
        {PROPERTIES.map(({ icon: Icon, title, text }) => (
          <li key={title} className="flex flex-col gap-2">
            <Icon className="size-4.5 text-foreground" aria-hidden="true" />
            <span className="mt-1 text-sm font-medium">{title}</span>
            <span className="text-sm leading-relaxed text-pretty text-muted-foreground">{text}</span>
          </li>
        ))}
      </ul>
    </section>
  );
}

export function SiteFooter() {
  const links = [
    { href: LINKS.repo, label: "GitHub" },
    { href: LINKS.selfHosting, label: "Self-hosting" },
    { href: LINKS.docs, label: "Docs" },
    { href: LINKS.reference, label: "API reference" },
    { href: LINKS.npm, label: "npm @phala/pay" },
    { href: LINKS.security, label: "Security" },
  ];
  return (
    <footer className="border-t">
      <div
        className={`${CONTAINER} flex flex-col gap-4 py-8 text-[0.8125rem] text-muted-foreground md:flex-row md:items-center md:justify-between`}
      >
        <p className="flex items-center gap-2">
          <Logo />
          <span>
            Phala Pay is open source under the{" "}
            <a className="underline underline-offset-4 hover:text-foreground" href={LINKS.license}>
              Apache-2.0 license
            </a>
            .
          </span>
        </p>
        <nav aria-label="Developers" className="flex flex-wrap items-center gap-x-6 gap-y-2">
          {links.map(({ href, label }) => (
            <a key={label} className="transition-colors hover:text-foreground" href={href}>
              {label}
            </a>
          ))}
        </nav>
      </div>
    </footer>
  );
}
