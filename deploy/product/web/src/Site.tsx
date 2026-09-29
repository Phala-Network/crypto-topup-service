import { ArrowRight, BookOpen, Braces, Cpu, GitBranch, Server, Wallet, type LucideIcon } from "lucide-react";
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
          <Button variant="ghost" asChild className="hidden hover:text-foreground sm:inline-flex">
            <a href={LINKS.docs}>Docs</a>
          </Button>
          <Button variant="ghost" asChild className="hidden hover:text-foreground sm:inline-flex">
            <a href={LINKS.reference}>API reference</a>
          </Button>
          <Button variant="ghost" asChild className="hidden hover:text-foreground md:inline-flex">
            <a href={LINKS.selfHosting}>Self-hosting</a>
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

// README.md's opening: what the product is. Self-hosting is how it is run, a secondary link.
export function Hero() {
  return (
    <section aria-labelledby="hero-title">
      <div className={`${CONTAINER} flex flex-col items-start gap-3 pt-8 pb-7 lg:pt-10 lg:pb-8`}>
        <h1
          id="hero-title"
          className="max-w-5xl text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl"
        >
          Non-custodial crypto payments with a Stripe-shaped API
        </h1>
        <p className="max-w-4xl text-base text-pretty text-muted-foreground sm:text-lg">
          Quotes, deposit addresses, refunds, and signed webhooks, where every address can only pay your treasury.
        </p>
        <div className="flex flex-wrap items-center gap-3 pt-2">
          <Button asChild size="lg">
            <a href={LINKS.docs}>
              <BookOpen aria-hidden="true" />
              Read the docs
            </a>
          </Button>
          <Button asChild size="lg" variant="outline">
            <a href={LINKS.repo}>
              <GitBranch aria-hidden="true" />
              GitHub
            </a>
          </Button>
          <a
            href={LINKS.selfHosting}
            className="inline-flex items-center gap-1 px-1 text-sm text-muted-foreground transition-colors hover:text-foreground"
          >
            Open source · Self-host it
            <ArrowRight className="size-3.5" aria-hidden="true" />
          </a>
        </div>
      </div>
    </section>
  );
}

export function Properties() {
  return (
    <section aria-labelledby="properties-title" className="border-t bg-muted/30">
      <div className={`${CONTAINER} flex flex-col gap-10 py-16 lg:py-20`}>
        <div className="flex max-w-2xl flex-col gap-2">
          <h2 id="properties-title" className="text-2xl font-semibold tracking-tight">
            Payments you can verify
          </h2>
          <p className="text-muted-foreground">
            The service never holds funds, and you can check what it runs before you trust it.
          </p>
        </div>
        <ul className="grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {PROPERTIES.map(({ icon: Icon, title, text }) => (
            <li key={title} className="flex flex-col gap-3 rounded-xl border bg-card p-5 text-card-foreground">
              <span className="flex size-9 items-center justify-center rounded-lg border bg-background" aria-hidden="true">
                <Icon className="size-4" />
              </span>
              <h3 className="text-sm font-semibold">{title}</h3>
              <p className="text-sm leading-relaxed text-pretty text-muted-foreground">{text}</p>
            </li>
          ))}
        </ul>
      </div>
    </section>
  );
}

const FOOTER: { title: string; links: { href: string; label: string }[] }[] = [
  {
    title: "Product",
    links: [
      { href: "#demo", label: "Live demo" },
      { href: LINKS.selfHosting, label: "Self-hosting" },
      { href: LINKS.security, label: "Security" },
    ],
  },
  {
    title: "Developers",
    links: [
      { href: LINKS.docs, label: "Integration guide" },
      { href: LINKS.reference, label: "API reference" },
      { href: LINKS.npm, label: "npm @phala/pay" },
    ],
  },
  {
    title: "Open source",
    links: [
      { href: LINKS.repo, label: "GitHub" },
      { href: LINKS.license, label: "Apache-2.0 license" },
    ],
  },
];

export function SiteFooter() {
  return (
    <footer className="border-t">
      <div className={`${CONTAINER} grid gap-10 py-12 text-sm sm:grid-cols-2 lg:grid-cols-[minmax(0,2fr)_repeat(3,minmax(0,1fr))]`}>
        <div className="flex flex-col gap-3">
          <span className="flex items-center gap-2 font-semibold tracking-tight">
            <Logo />
            Phala Pay
          </span>
          <p className="max-w-xs text-muted-foreground">
            Non-custodial crypto payments with a Stripe-shaped API. Open source, self-hosted.
          </p>
        </div>
        {FOOTER.map((column) => (
          <nav key={column.title} aria-label={column.title} className="flex flex-col gap-3">
            <h2 className="font-medium">{column.title}</h2>
            <ul className="flex flex-col gap-2">
              {column.links.map(({ href, label }) => (
                <li key={label}>
                  <a className="text-muted-foreground transition-colors hover:text-foreground" href={href}>
                    {label}
                  </a>
                </li>
              ))}
            </ul>
          </nav>
        ))}
      </div>
      <div className="border-t">
        <p className={`${CONTAINER} py-6 text-xs text-muted-foreground`}>
          The demo above runs on testnets with test tokens; no real money moves.
        </p>
      </div>
    </footer>
  );
}
