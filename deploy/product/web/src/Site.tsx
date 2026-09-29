import { BookOpen, Braces, Cpu, Menu, Rocket, Server, Wallet, type LucideIcon } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Sheet, SheetClose, SheetContent, SheetHeader, SheetTitle, SheetTrigger } from "@/components/ui/sheet";
import { GitHubIcon } from "./common.js";
import phaMark from "./icons/pha.svg";
import { ICON_BUTTON, ThemeToggle, type Theme } from "./theme.js";

const REPO = "https://github.com/Phala-Network/phala-pay";
const LINKS = {
  repo: REPO,
  docs: `${REPO}/blob/main/docs/integration.md`,
  selfHosting: `${REPO}/blob/main/docs/self-hosting.md`,
  // The guide's first step: fork the repository and deploy it to your own Phala Cloud workspace.
  deploy: `${REPO}/blob/main/docs/self-hosting.md#1-prerequisites`,
  reference: "https://phala-network.github.io/phala-pay/",
  npm: "https://www.npmjs.com/package/@phala/pay",
  license: `${REPO}/blob/main/LICENSE`,
  security: `${REPO}/blob/main/SECURITY.md`,
};

/** The page's width. */
export const CONTAINER = "mx-auto w-full max-w-[84rem] px-5 sm:px-8 2xl:max-w-[92rem]";

// The headline, as index.html's title, description, and link preview (brand/og-image.svg) carry it.
const TAGLINE = "Fast, secure, non-custodial crypto payments";

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

const NAV = [
  { href: LINKS.docs, label: "Docs" },
  { href: LINKS.reference, label: "API reference" },
  { href: LINKS.selfHosting, label: "Self-hosting" },
];

export function SiteHeader({ theme, onThemeChange }: { theme: Theme; onThemeChange: (theme: Theme) => void }) {
  return (
    <header className="sticky top-0 z-50 border-b bg-background">
      <div className={`${CONTAINER} flex h-14 items-center justify-between gap-4`}>
        <a href="#top" className="flex items-center gap-2 rounded-md text-[0.9375rem] font-semibold tracking-tight">
          <Logo />
          Phala Pay
        </a>
        <nav aria-label="Site" className="-mr-3 flex items-center gap-1 text-muted-foreground">
          {NAV.map(({ href, label }) => (
            <Button key={label} variant="ghost" asChild className="hidden hover:text-foreground md:inline-flex">
              <a href={href}>{label}</a>
            </Button>
          ))}
          <a href={LINKS.repo} aria-label="GitHub" className={ICON_BUTTON}>
            <GitHubIcon />
          </a>
          <ThemeToggle theme={theme} onChange={onThemeChange} />
          <Sheet>
            <SheetTrigger asChild>
              <button type="button" className={`${ICON_BUTTON} md:hidden`} aria-label="Menu">
                <Menu aria-hidden="true" />
              </button>
            </SheetTrigger>
            <SheetContent side="right" className="w-72">
              <SheetHeader>
                <SheetTitle>Phala Pay</SheetTitle>
              </SheetHeader>
              <nav aria-label="Menu" className="flex flex-col gap-1 px-4">
                {NAV.map(({ href, label }) => (
                  <SheetClose asChild key={label}>
                    <a className="rounded-md px-2 py-2 text-sm font-medium hover:bg-accent" href={href}>
                      {label}
                    </a>
                  </SheetClose>
                ))}
              </nav>
            </SheetContent>
          </Sheet>
        </nav>
      </div>
    </header>
  );
}

/** Phala's "P" mark (./icons/pha.svg), the Phala Pay logo, as in the favicon and link preview. */
function Logo() {
  return <img src={phaMark} alt="" className="size-6 rounded-md ring-1 ring-foreground/10" />;
}

// The hero's two calls to action: one height, whatever their variant.
const HERO_BUTTON = "h-10 px-4";

// The headline, with the fact behind each of its words (docs/architecture.md §8, the typical credit
// at depth 2; README.md), and the way to run it: self-hosting on Phala Cloud.
export function Hero() {
  return (
    <section aria-labelledby="hero-title">
      <div className={`${CONTAINER} flex flex-col items-start gap-3 pt-8 pb-7 lg:pt-10 lg:pb-8`}>
        <h1 id="hero-title" className="max-w-5xl text-3xl leading-tight font-semibold tracking-tight text-balance sm:text-4xl">
          {TAGLINE}
        </h1>
        <p className="max-w-2xl text-base text-pretty text-muted-foreground sm:text-lg">
          {/* Each sentence a line where the paragraph is wide; on a phone, the no-break spaces keep its
              wraps between clauses. */}
          Credited at two confirmations, about 15&nbsp;s on&nbsp;Ethereum, in a TEE you can verify. Payment&nbsp;addresses
          can only pay your treasury, behind a Stripe-shaped API.
        </p>
        <div className="flex flex-wrap items-center gap-3 pt-2">
          <Button asChild size="lg" className={HERO_BUTTON}>
            <a href={LINKS.deploy}>
              <Rocket aria-hidden="true" />
              Deploy on Phala Cloud
            </a>
          </Button>
          <Button asChild size="lg" variant="outline" className={HERO_BUTTON}>
            <a href={LINKS.docs}>
              <BookOpen aria-hidden="true" />
              Docs
            </a>
          </Button>
        </div>
      </div>
    </section>
  );
}

export function Properties() {
  return (
    <section aria-labelledby="properties-title" className="border-t bg-muted/30">
      <div className={`${CONTAINER} py-16`}>
        <h2 id="properties-title" className="text-2xl font-semibold tracking-tight">
          Payments you can verify
        </h2>
        <p className="mt-2 max-w-2xl leading-6 text-muted-foreground">
          The service never holds funds, and you can check what it runs before you trust it.
        </p>
        <ul className="mt-10 grid gap-4 sm:grid-cols-2 lg:grid-cols-4">
          {PROPERTIES.map(({ icon: Icon, title, text }) => (
            <li key={title} className="rounded-xl border bg-card p-6 text-card-foreground">
              <span className="flex size-9 items-center justify-center rounded-lg border bg-background" aria-hidden="true">
                <Icon className="size-4" />
              </span>
              <h3 className="mt-4 text-sm font-semibold">{title}</h3>
              <p className="mt-2 text-sm leading-6 text-pretty text-muted-foreground">{text}</p>
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
      <div className={`${CONTAINER} grid gap-10 py-16 text-sm sm:grid-cols-2 lg:grid-cols-[minmax(0,2fr)_repeat(3,minmax(0,1fr))]`}>
        <div>
          <span className="flex items-center gap-2 font-semibold tracking-tight">
            <Logo />
            Phala Pay
          </span>
          <p className="mt-3 max-w-xs leading-6 text-muted-foreground">{TAGLINE}</p>
        </div>
        {FOOTER.map((column) => (
          <nav key={column.title} aria-label={column.title}>
            <h2 className="font-medium">{column.title}</h2>
            <ul className="mt-3 flex flex-col gap-2">
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
        <div
          className={`${CONTAINER} flex flex-col gap-2 py-6 text-xs text-muted-foreground sm:flex-row sm:items-center sm:justify-between`}
        >
          <p>© 2026 Phala Network</p>
          <p>The demo above runs on testnets with test tokens; no real money moves.</p>
        </div>
      </div>
    </footer>
  );
}
