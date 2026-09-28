import {
  ArrowRight,
  BadgeCheck,
  BookOpen,
  Braces,
  CircleDollarSign,
  Coins,
  Cpu,
  FileText,
  FlaskConical,
  GitBranch,
  KeyRound,
  Package,
  QrCode,
  Receipt,
  Undo2,
  Wallet,
  Webhook,
  type LucideIcon,
} from "lucide-react";
import type { ReactNode } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import { ThemeToggle, useTheme } from "../theme.js";

const REPO = "https://github.com/Phala-Network/phala-pay";
const LINKS = {
  repo: REPO,
  readme: `${REPO}#readme`,
  guide: `${REPO}/blob/main/docs/integration.md`,
  reference: "https://phala-network.github.io/phala-pay/",
  npm: "https://www.npmjs.com/package/@phala/pay",
  security: `${REPO}/blob/main/SECURITY.md`,
  license: `${REPO}/blob/main/LICENSE`,
};
// Relative, so the site works under any path.
const DEMO = "./demo/";

const CONTAINER = "mx-auto w-full max-w-7xl px-4 sm:px-6 lg:px-8";

const NAV = [
  { href: "#how-it-works", label: "How it works" },
  { href: "#properties", label: "Properties" },
  { href: "#integrate", label: "Integrate" },
  { href: "#developers", label: "Developers" },
];

// README.md, "Flow".
const FLOW: { icon: LucideIcon; title: string; text: string }[] = [
  {
    icon: Receipt,
    title: "Your backend creates a quote",
    text: "With its API key: a locked price, an exact amount, and a single-use address.",
  },
  {
    icon: BadgeCheck,
    title: "Your page shows it",
    text: "Only after the SDK recomputed the address from your own pins.",
  },
  {
    icon: QrCode,
    title: "The customer pays",
    text: "From a browser wallet, by QR code, or by a manual transfer.",
  },
  {
    icon: Webhook,
    title: "You credit it",
    text: "At two confirmations, priced and screened: a signed deposit.credited webhook.",
  },
  {
    icon: Wallet,
    title: "You sweep",
    text: "Forwarders pay only your treasury, when you flush them.",
  },
];

// README.md; docs/architecture.md §1; docs/integration.md §1.1, §1.7, §3, §5.2–§5.6.
const PROPERTIES: { icon: LucideIcon; title: string; text: ReactNode }[] = [
  {
    icon: Wallet,
    title: "Non-custodial",
    text: (
      <>
        Deposit addresses are CREATE2 forwarder contracts that can only pay your own treasury. The
        service holds no funds and sends no transactions: software, not custody.
      </>
    ),
  },
  {
    icon: Coins,
    title: "No fees",
    text: (
      <>
        Phala Pay charges no fee. You pay the gas of your own sweeps and refunds, and payers pay the
        gas of their payments.
      </>
    ),
  },
  {
    icon: Cpu,
    title: "Runs in a TEE",
    text: (
      <>
        The service runs inside a dstack confidential VM. Verify its attestation, with the official
        dstack verifier, before you trust it.
      </>
    ),
  },
  {
    icon: KeyRound,
    title: "Per-account webhook keys",
    text: (
      <>
        Each account's events are signed with its own ed25519 key, derived inside the VM and pinned
        from attestation. You hold only the public key, so nothing you store can forge a credit.
      </>
    ),
  },
  {
    icon: Braces,
    title: "Stripe-shaped API",
    text: (
      <>
        Secret and restricted API keys, <code>Idempotency-Key</code> replays, <code>metadata</code>,
        Stripe's Event object, and Standard Webhooks signatures.
      </>
    ),
  },
  {
    icon: BadgeCheck,
    title: "Fail-closed addresses",
    text: (
      <>
        The SDKs recompute every address from pins you configure. When the service names another
        address, the checkout shows nothing to pay.
      </>
    ),
  },
];

// docs/integration.md, Quickstart; sdk/js/README.md, "Server helpers".
const CHECKOUT = `import { Checkout } from "@phala/pay/react";

// Your backend created the quote with its API key and returned only its
// client secret and the address the SDK recomputed from your pins.
<Checkout
  clientSecret={clientSecret}
  expectedAddress={expectedAddress}
  apiBase={PHALA_PAY_API_BASE}
  onSuccess={() => router.refresh()}
  onExpire={() => startOver()}
/>`;

const WEBHOOK_PYTHON = `from phala_pay import SignatureVerificationError

@app.post("/webhooks/phala-pay")
async def webhook(request: Request) -> Response:
    try:
        event = pay.webhooks.construct_event(
            await request.body(), request.headers, WEBHOOK_KEYS, ACCOUNT, expected_livemode=False
        )
    except (SignatureVerificationError, ValueError):
        return Response(status_code=400)
    if event.type.startswith("deposit."):
        # Credit, refund, and reversal alike: merge the snapshot and move the balance by the
        # change in what the deposit nets to, in one transaction per deposit.
        apply_deposit(event.deposit)
    return Response(status_code=200)`;

const WEBHOOK_NODE = `import { constructEvent } from "@phala/pay/server";

// Standard Webhooks v1a (ed25519, WebCrypto: Node 20+, Deno, Bun, edge runtimes). Fails closed
// unless the signature verifies with a pinned key and the event is your account's in this mode.
const event = await constructEvent(rawBody, request.headers, WEBHOOK_PUBLIC_KEYS, {
  expectedAccount: "acct_…",
  expectedLivemode: false,
});`;

const DEVELOPER_LINKS: { icon: LucideIcon; title: string; text: string; href: string }[] = [
  {
    icon: GitBranch,
    title: "GitHub",
    text: "The service, the SDKs, the contracts, and the design and architecture documents.",
    href: LINKS.repo,
  },
  {
    icon: BookOpen,
    title: "Integration guide",
    text: "Quickstart, quotes, deposit addresses, treasuries, sweeps, webhooks, refunds, and keys.",
    href: LINKS.guide,
  },
  {
    icon: FileText,
    title: "API reference",
    text: "Every request and response, with examples, built from the OpenAPI document.",
    href: LINKS.reference,
  },
  {
    icon: Package,
    title: "npm: @phala/pay",
    text: "The browser checkout for React, a framework-agnostic core, and server helpers.",
    href: LINKS.npm,
  },
];

export function Landing() {
  const [theme, setTheme] = useTheme();
  return (
    <div className="flex min-h-svh flex-col">
      <header className="sticky top-0 z-30 border-b bg-background/90 backdrop-blur">
        <div className={`${CONTAINER} flex h-14 items-center justify-between gap-4`}>
          <a href="#top" className="flex items-center gap-2 font-semibold">
            <span className="flex size-7 items-center justify-center rounded-lg bg-primary text-primary-foreground">
              <CircleDollarSign className="size-4" aria-hidden="true" />
            </span>
            Phala Pay
          </a>
          <nav aria-label="Sections" className="hidden items-center gap-1 md:flex">
            {NAV.map((item) => (
              <Button key={item.href} variant="ghost" asChild>
                <a href={item.href}>{item.label}</a>
              </Button>
            ))}
          </nav>
          <div className="flex items-center gap-2">
            <Button variant="outline" asChild className="hidden sm:inline-flex">
              <a href={LINKS.repo}>
                <GitBranch aria-hidden="true" />
                GitHub
              </a>
            </Button>
            <ThemeToggle theme={theme} onChange={setTheme} />
          </div>
        </div>
      </header>

      <main id="top" className="flex-1">
        <section className="border-b bg-muted/40" aria-labelledby="hero-title">
          <div className={`${CONTAINER} grid items-center gap-12 py-16 lg:grid-cols-[minmax(0,1.1fr)_minmax(0,1fr)] lg:py-24`}>
            <div className="flex flex-col items-start gap-6">
              <Badge variant="outline" className="gap-1.5">
                <FlaskConical aria-hidden="true" />
                Sepolia testnet demo · mainnet is not live yet
              </Badge>
              <h1 id="hero-title" className="text-4xl font-semibold tracking-tight sm:text-5xl">
                Phala Pay
              </h1>
              <p className="max-w-xl text-xl text-foreground/90 sm:text-2xl">
                Crypto payments in Stripe's shape, without custody.
              </p>
              <p className="max-w-xl text-muted-foreground">
                An API-only payments service. Your backend creates quotes and deposit addresses with its
                API keys, your customers pay supported ERC-20 tokens to addresses that can only pay your
                treasury, and a signed webhook tells you what to credit.
              </p>
              <div className="flex flex-wrap gap-3">
                <Button size="lg" asChild>
                  <a href={DEMO}>
                    Try the demo
                    <ArrowRight data-icon="inline-end" aria-hidden="true" />
                  </a>
                </Button>
                <Button size="lg" variant="outline" asChild>
                  <a href={LINKS.readme}>
                    <BookOpen data-icon="inline-start" aria-hidden="true" />
                    Read the docs
                  </a>
                </Button>
              </div>
            </div>
            <Card>
              <CardHeader>
                <CardTitle>
                  <h2>A payment, end to end</h2>
                </CardTitle>
                <CardDescription>Typically about 30 seconds from payment to credit on Ethereum.</CardDescription>
              </CardHeader>
              <CardContent>
                <ol className="flex flex-col gap-4">
                  {FLOW.map(({ icon: Icon, title, text }) => (
                    <li key={title} className="flex gap-3">
                      <span className="flex size-8 shrink-0 items-center justify-center rounded-lg bg-muted">
                        <Icon className="size-4" aria-hidden="true" />
                      </span>
                      <div className="flex flex-col gap-0.5">
                        <span className="font-medium">{title}</span>
                        <span className="text-muted-foreground">{text}</span>
                      </div>
                    </li>
                  ))}
                </ol>
              </CardContent>
            </Card>
          </div>
        </section>

        <Section
          id="how-it-works"
          title="How it works"
          lead="Two ways to collect a payment, one way to credit it, and the money never leaves your control."
        >
          <div className="grid gap-6 md:grid-cols-2">
            <Card>
              <CardHeader>
                <CardTitle>
                  <h3>Quote: an exact amount at a locked price</h3>
                </CardTitle>
              </CardHeader>
              <CardContent className="text-muted-foreground">
                The customer states an amount in dollars and receives a locked price, an exact token
                amount, and a single-use address to pay within the window, like a PaymentIntent. A late
                payment or another amount is still credited, at the market price.
              </CardContent>
            </Card>
            <Card>
              <CardHeader>
                <CardTitle>
                  <h3>Deposit address: any amount, at any time</h3>
                </CardTitle>
              </CardHeader>
              <CardContent className="text-muted-foreground">
                Each customer can have one persistent, rotatable address for every supported token on
                every supported chain, credited at spot for any amount, like the bank-transfer details of
                Stripe's customer balance.
              </CardContent>
            </Card>
          </div>
          <div className="grid gap-6 md:grid-cols-3">
            <Step icon={Webhook} title="Credited at two confirmations">
              The service confirms each payment with a second RPC provider, prices and screens it, and
              sends a signed <code>deposit.credited</code>, typically about 30 seconds after paying. It
              watches the deposit to finality; a dropped transaction becomes <code>deposit.reversed</code>.
            </Step>
            <Step icon={Wallet} title="You sweep">
              Payments wait in their forwarders until you sweep them with one{" "}
              <code>factory.flush</code> from your own wallet or Safe. Anyone may send it: the funds can
              only reach your treasury.
            </Step>
            <Step icon={Undo2} title="You refund">
              Declare a refund, pay it from your treasury, and attach the transaction. The service
              verifies it once final and sends <code>deposit.refunded</code>; it never moves funds.
            </Step>
          </div>
        </Section>

        <Section
          id="properties"
          title="Key properties"
          lead="What the service does not do matters as much as what it does."
          muted
        >
          <div className="grid gap-6 sm:grid-cols-2 lg:grid-cols-3">
            {PROPERTIES.map(({ icon: Icon, title, text }) => (
              <Card key={title}>
                <CardHeader>
                  <span className="flex size-9 items-center justify-center rounded-lg bg-muted">
                    <Icon className="size-4.5" aria-hidden="true" />
                  </span>
                  <CardTitle className="pt-2">
                    <h3>{title}</h3>
                  </CardTitle>
                </CardHeader>
                <CardContent className="text-muted-foreground">{text}</CardContent>
              </Card>
            ))}
          </div>
        </Section>

        <Section
          id="integrate"
          title="Integrate"
          lead="Three pieces, as with Stripe's Payment Element: your backend creates a quote, the browser renders the checkout with its client secret, and your webhook handler credits the deposit."
        >
          <div className="grid items-start gap-8 lg:grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)]">
            <div className="flex flex-col gap-4 text-muted-foreground">
              <p>
                <span className="font-medium text-foreground">The checkout</span>, <code>&lt;Checkout&gt;</code>{" "}
                from <code>@phala/pay</code>, offers a browser wallet, a QR code, and manual payment, with
                live status until the payment is credited. <code>expectedAddress</code> is required: the
                checkout fails closed when the quote names another address.
              </p>
              <p>
                <span className="font-medium text-foreground">The webhook</span> is the only trusted
                source of payment: verify its signature over the raw body, then apply the deposit once.
                The Python SDK (<code>phala-pay</code>) and <code>@phala/pay/server</code> both fail
                closed.
              </p>
              <p>
                The SDKs for this API are not released yet: the{" "}
                <a className="font-medium text-foreground underline underline-offset-4" href={LINKS.guide}>
                  integration guide
                </a>{" "}
                shows how to install them from the repository.
              </p>
            </div>
            <Tabs defaultValue="checkout" className="min-w-0">
              <TabsList aria-label="Code sample" className="max-w-full justify-start overflow-x-auto">
                <TabsTrigger value="checkout">Checkout (React)</TabsTrigger>
                <TabsTrigger value="python">Webhook (Python)</TabsTrigger>
                <TabsTrigger value="node">Webhook (Node.js)</TabsTrigger>
              </TabsList>
              <TabsContent value="checkout">
                <Code>{CHECKOUT}</Code>
              </TabsContent>
              <TabsContent value="python">
                <Code>{WEBHOOK_PYTHON}</Code>
              </TabsContent>
              <TabsContent value="node">
                <Code>{WEBHOOK_NODE}</Code>
              </TabsContent>
            </Tabs>
          </div>
        </Section>

        <Section id="developers" title="Developers" lead="Everything is open source, in one repository." muted>
          <div className="grid gap-6 sm:grid-cols-2 lg:grid-cols-4">
            {DEVELOPER_LINKS.map(({ icon: Icon, title, text, href }) => (
              <a key={title} href={href} className="group rounded-xl focus-visible:ring-3 focus-visible:ring-ring/50 focus-visible:outline-none">
                <Card className="h-full transition-colors group-hover:bg-muted/50">
                  <CardHeader>
                    <Icon className="size-5" aria-hidden="true" />
                    <CardTitle className="flex items-center gap-1 pt-2">
                      <h3>{title}</h3>
                      <ArrowRight className="size-4 transition-transform group-hover:translate-x-0.5" aria-hidden="true" />
                    </CardTitle>
                    <CardDescription>{text}</CardDescription>
                  </CardHeader>
                </Card>
              </a>
            ))}
          </div>
          <Alert>
            <FlaskConical aria-hidden="true" />
            <AlertTitle>Testnet only</AlertTitle>
            <AlertDescription>
              <p>
                The demo runs on the Sepolia testnet with test PHA: no real money moves. Mainnet is not
                live yet.{" "}
                <a href={DEMO} className="font-medium text-foreground">
                  Try the demo
                </a>
                : a cloud console's billing page, paid with Phala Pay, that shows each payment's path
                through the chain, the service, and the console's webhook handler.
              </p>
            </AlertDescription>
          </Alert>
        </Section>
      </main>

      <footer className="border-t">
        <div className={`${CONTAINER} flex flex-col gap-4 py-8 text-sm text-muted-foreground md:flex-row md:items-center md:justify-between`}>
          <p>
            Phala Pay is open source under the{" "}
            <a className="underline underline-offset-4 hover:text-foreground" href={LINKS.license}>
              Apache-2.0 license
            </a>
            .
          </p>
          <nav aria-label="Footer" className="flex flex-wrap items-center gap-x-4 gap-y-2">
            <a className="hover:text-foreground" href={DEMO}>
              Demo
            </a>
            <a className="hover:text-foreground" href={LINKS.repo}>
              GitHub
            </a>
            <a className="hover:text-foreground" href={LINKS.reference}>
              API reference
            </a>
            <a className="hover:text-foreground" href={LINKS.npm}>
              npm
            </a>
            <a className="hover:text-foreground" href={LINKS.security}>
              Security
            </a>
          </nav>
        </div>
      </footer>
    </div>
  );
}

function Section({
  id,
  title,
  lead,
  muted = false,
  children,
}: {
  id: string;
  title: string;
  lead: string;
  muted?: boolean;
  children: ReactNode;
}) {
  return (
    <section id={id} aria-labelledby={`${id}-title`} className={cn("scroll-mt-14", muted && "border-y bg-muted/40")}>
      <div className={`${CONTAINER} flex flex-col gap-8 py-16 lg:py-20`}>
        <div className="flex max-w-3xl flex-col gap-3">
          <h2 id={`${id}-title`} className="text-3xl font-semibold tracking-tight">
            {title}
          </h2>
          <p className="text-lg text-muted-foreground">{lead}</p>
        </div>
        {children}
      </div>
    </section>
  );
}

function Step({ icon: Icon, title, children }: { icon: LucideIcon; title: string; children: ReactNode }) {
  return (
    <div className="flex flex-col gap-2">
      <h3 className="flex items-center gap-2 font-medium">
        <Icon className="size-4 text-muted-foreground" aria-hidden="true" />
        {title}
      </h3>
      <p className="text-muted-foreground">{children}</p>
    </div>
  );
}

function Code({ children }: { children: string }) {
  return (
    <pre className="overflow-x-auto rounded-xl bg-muted p-4 font-mono text-xs leading-relaxed">
      <code>{children}</code>
    </pre>
  );
}
