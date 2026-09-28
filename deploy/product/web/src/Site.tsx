import { ArrowRight, Braces, BookOpen, Coins, Cpu, GitBranch, Wallet, type LucideIcon } from "lucide-react";
import { useState } from "react";
import { Button } from "@/components/ui/button";
import type { Account } from "./api.js";
import { ExplorerLink, InfoTip, errorMessage, wallet } from "./common.js";
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

/** The page's width. */
export const CONTAINER = "mx-auto w-full max-w-[84rem] px-5 sm:px-8 2xl:max-w-[92rem]";

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

export function Hero({ account }: { account: Account | null }) {
  return (
    <section aria-labelledby="hero-title">
      <div className={`${CONTAINER} flex flex-col gap-8 pt-12 pb-10 sm:pt-16 lg:flex-row lg:items-end lg:justify-between lg:gap-16 lg:pt-20 lg:pb-14`}>
        <div className="flex flex-col items-start gap-6">
          <TestnetNote account={account} />
          <h1
            id="hero-title"
            className="text-[2.75rem] leading-[1.02] font-semibold tracking-[-0.04em] text-balance sm:text-6xl xl:text-7xl"
          >
            Crypto payments,{" "}
            <br />
            <span className="text-muted-foreground">without custody.</span>
          </h1>
        </div>
        <div className="flex max-w-md flex-col gap-6 lg:pb-2">
          <p className="text-lg leading-relaxed text-pretty text-muted-foreground">
            Your customers pay addresses that can only pay your treasury, and a signed webhook tells you
            what to credit.
          </p>
          <div className="flex flex-wrap gap-3">
            <Button asChild size="lg" className="h-10 gap-2 rounded-full px-5">
              <a href={LINKS.docs}>
                <BookOpen aria-hidden="true" />
                Read the docs
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

/** The testnet notice: one slim line, with the test token's mint. */
function TestnetNote({ account }: { account: Account | null }) {
  const [state, setState] = useState<{ kind: "idle" | "pending" | "done" | "failed"; text?: string }>({
    kind: "idle",
  });
  const symbol = account?.token.symbol ?? "PHA";
  const network = account?.network.name ?? "Sepolia";
  const mint = async (current: Account) => {
    setState({ kind: "pending" });
    try {
      const { mintTestTokens } = await wallet();
      const hash = await mintTestTokens(current.network.chain_id, current.token.address, "1000");
      setState({ kind: "done", text: hash });
    } catch (error) {
      setState({ kind: "failed", text: errorMessage(error, "Minting failed.") });
    }
  };
  return (
    <div role="note" aria-label="Testnet demo" className="flex flex-wrap items-center gap-x-4 gap-y-2 text-xs">
      <span className="flex items-center gap-2 rounded-full border bg-card/60 py-1 pr-2.5 pl-1 text-muted-foreground shadow-xs">
        <span className="rounded-full bg-muted px-2 py-0.5 font-medium text-foreground">Testnet</span>
        <span>Sepolia testnet · mainnet not live yet</span>
        <InfoTip label="About the testnet demo" className="translate-y-0">
          {network} and test {symbol} only; no real money moves. Test {symbol} is free: mint it from your wallet
          (gas is {network} ETH from a public faucet).
        </InfoTip>
      </span>
      {account?.network.testnet === true && (
        <button
          type="button"
          className="group/mint inline-flex items-center gap-1 rounded-sm font-medium outline-none hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60"
          onClick={() => void mint(account)}
          disabled={state.kind === "pending"}
        >
          {state.kind === "pending" ? "Confirm in your wallet…" : `Get 1,000 test ${symbol}`}
          <ArrowRight
            className="size-3 transition-transform group-hover/mint:translate-x-0.5 motion-reduce:transition-none"
            aria-hidden="true"
          />
        </button>
      )}
      <span aria-live="polite" className="text-muted-foreground empty:hidden">
        {state.kind === "done" && state.text !== undefined && (
          <>
            Minted: <ExplorerLink account={account} kind="tx" value={state.text} />
          </>
        )}
        {state.kind === "failed" && <span className="text-destructive">{state.text}</span>}
      </span>
    </div>
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
