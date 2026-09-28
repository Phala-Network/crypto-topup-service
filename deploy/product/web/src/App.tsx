import { Checkout, type Appearance } from "@phala/pay/react";
import { ChevronRight, CircleAlert, Cloud, Cpu, ShieldCheck, TriangleAlert, Wallet } from "lucide-react";
import { useCallback, useEffect, useId, useState, type FormEvent } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from "@/components/ui/card";
import { Field, FieldLabel, FieldLegend, FieldSet, FieldTitle } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupInput, InputGroupText } from "@/components/ui/input-group";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { TooltipProvider } from "@/components/ui/tooltip";
import {
  createQuote,
  getAccount,
  getTimeline,
  getTrust,
  type Account,
  type CreatedQuote,
  type Selection,
  type Timeline,
  type Trust,
} from "./api.js";
import { Detail, Details, ExplorerLink, LINK, StatusBadge, describe, usePolling } from "./common.js";
import { DepositAddressPanel } from "./DepositAddressPanel.js";
import { dollars, short, signedDollars, statusLabel, time, tokens } from "./format.js";
import { Sweeps } from "./Sweeps.js";
import { errorMessage, mintTestTokens } from "./testTokens.js";
import { ThemeToggle, useTheme } from "./theme.js";
import { BehindTheScenes } from "./Timeline.js";

type Method = "quote" | "address";

interface Session {
  quote: string;
  clientSecret: string;
  expectedAddress: string;
  orderId: string;
}

const METHODS: { id: Method; label: string }[] = [
  { id: "quote", label: "Exact amount" },
  { id: "address", label: "Deposit address" },
];

/** The page's width: wide screens get room for the payment and its timeline side by side. */
const CONTAINER = "mx-auto w-full max-w-[1760px] px-4 sm:px-6 lg:px-8";

export function App() {
  const [theme, setTheme] = useTheme();
  const [account, setAccount] = useState<Account | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);
  const [method, setMethod] = useState<Method>("quote");
  const [session, setSession] = useState<Session | null>(null);
  const [selected, setSelected] = useState<Selection | null>(null);
  const [timeline, setTimeline] = useState<{ key: string; view: Timeline } | null>(null);
  const [trust, setTrust] = useState<Trust | null>(null);

  const refreshAccount = useCallback(() => {
    getAccount().then(
      (next) => {
        setAccount(next);
        setAccountError(null);
      },
      (error: unknown) => setAccountError(describe(error)),
    );
  }, []);
  usePolling(refreshAccount, 4000);

  useEffect(() => {
    getTrust().then(setTrust, () => setTrust(null));
  }, []);

  const selectedKey = selected === null ? null : `${selected.kind}:${selected.id}`;
  const refreshTimeline = useCallback(() => {
    if (selected === null) {
      return;
    }
    const key = `${selected.kind}:${selected.id}`;
    getTimeline(selected).then(
      (view) => setTimeline({ key, view }),
      () => undefined,
    );
  }, [selected]);
  usePolling(refreshTimeline, 3000, selected !== null);
  const refreshAll = () => {
    refreshTimeline();
    refreshAccount();
  };

  // The SDK's components take the page's theme tokens (src/index.css).
  const appearance: Appearance = {
    theme,
    variables: {
      colorPrimary: "var(--primary)",
      accessibleColorOnColorPrimary: "var(--primary-foreground)",
      colorBackground: "var(--card)",
      colorText: "var(--card-foreground)",
      colorTextSecondary: "var(--muted-foreground)",
      colorBorder: "var(--border)",
      colorDanger: "var(--destructive)",
      colorSuccess: "var(--success)",
      borderRadius: "var(--radius)",
      fontFamily: "inherit",
    },
  };
  const select = (next: Selection) => {
    setSelected(next);
    setTimeline(null);
  };

  return (
    <TooltipProvider>
      <div className="min-h-svh bg-muted/40 dark:bg-background">
        <header className="sticky top-0 z-30 border-b bg-background/90 backdrop-blur">
          <div className={`${CONTAINER} flex h-14 items-center justify-between gap-4`}>
            <div className="flex min-w-0 items-center gap-2 text-sm font-semibold">
              <span className="flex size-7 shrink-0 items-center justify-center rounded-lg bg-primary text-primary-foreground">
                <Cloud className="size-4" aria-hidden="true" />
              </span>
              <span className="truncate">Cloud Console</span>
              <span className="hidden font-normal text-muted-foreground sm:inline">/ Billing</span>
              <Badge variant="outline" asChild>
                <a href="../">Phala Pay demo</a>
              </Badge>
            </div>
            <ThemeToggle theme={theme} onChange={setTheme} />
          </div>
        </header>

        <main className={`${CONTAINER} flex flex-col gap-6 py-6 lg:py-8`}>
          {account?.network.testnet === true && <TestnetBanner account={account} />}
          <div className="flex flex-col gap-2">
            <h1 className="text-2xl font-semibold tracking-tight">Add credits</h1>
            <p className="max-w-3xl text-sm text-muted-foreground">
              A cloud console's billing page paid with Phala Pay: top up this workspace with{" "}
              {account?.token.symbol ?? "PHA"} on {account?.network.name ?? "Sepolia"}, either for an
              exact amount at a locked price, or at any time to your own deposit address. The balance
              moves only when this console's webhook handler receives a verified <code>deposit.*</code>{" "}
              event, exactly as a real integration applies credits.
            </p>
          </div>
          {accountError !== null && (
            <Alert variant="destructive">
              <CircleAlert aria-hidden="true" />
              <AlertDescription>Could not load the account: {accountError}</AlertDescription>
            </Alert>
          )}

          <div className="grid items-start gap-6 lg:grid-cols-[minmax(0,24rem)_minmax(0,1fr)] xl:grid-cols-[minmax(0,28rem)_minmax(0,1fr)] 2xl:grid-cols-[minmax(0,30rem)_minmax(0,1fr)]">
            <div className="flex min-w-0 flex-col gap-6">
              <BalanceCard account={account} />
              <Card role="region" aria-labelledby="pay-title">
                <CardHeader>
                  <CardTitle>
                    <h2 id="pay-title">Pay with crypto</h2>
                  </CardTitle>
                </CardHeader>
                <CardContent>
                  <Tabs value={method} onValueChange={(value) => setMethod(value === "address" ? "address" : "quote")}>
                    <TabsList aria-label="Payment method" className="w-full">
                      {METHODS.map(({ id, label }) => (
                        <TabsTrigger key={id} value={id}>
                          {label}
                        </TabsTrigger>
                      ))}
                    </TabsList>
                    <TabsContent value="quote" className="pt-3">
                      {session === null || account === null ? (
                        <AmountPicker
                          account={account}
                          onQuote={(created: CreatedQuote) => {
                            setSession({
                              quote: created.quote,
                              clientSecret: created.client_secret,
                              expectedAddress: created.expected_address,
                              orderId: created.order_id,
                            });
                            select({ kind: "quote", id: created.quote });
                          }}
                        />
                      ) : (
                        <div className="flex flex-col gap-4">
                          <p className="text-xs text-muted-foreground">
                            Order <code>{session.orderId}</code>, in the quote's <code>metadata</code>. The
                            checkout shows the quote only if the service's address is the one the product's
                            SDK recomputed from its pins.
                          </p>
                          <Checkout
                            clientSecret={session.clientSecret}
                            expectedAddress={session.expectedAddress}
                            apiBase={account.api_base}
                            appearance={appearance}
                            onSuccess={refreshAccount}
                          />
                          <Button type="button" variant="outline" onClick={() => setSession(null)}>
                            Start a new top-up
                          </Button>
                        </div>
                      )}
                    </TabsContent>
                    <TabsContent value="address" className="pt-3">
                      {account === null ? (
                        <p className="text-muted-foreground">Loading…</p>
                      ) : (
                        <DepositAddressPanel account={account} appearance={appearance} onSelect={select} />
                      )}
                    </TabsContent>
                  </Tabs>
                </CardContent>
              </Card>
            </div>
            <BehindTheScenes
              timeline={timeline !== null && timeline.key === selectedKey ? timeline.view : null}
              loading={selected?.id ?? null}
              account={account}
              onChanged={refreshAll}
            />
          </div>

          <Payments account={account} selected={selected} onSelect={select} />
          {account !== null && <Sweeps account={account} />}
          <TrustStrip trust={trust} account={account} />
        </main>
      </div>
    </TooltipProvider>
  );
}

function TestnetBanner({ account }: { account: Account }) {
  const [state, setState] = useState<{ kind: "idle" | "pending" | "done" | "failed"; text?: string }>({
    kind: "idle",
  });
  const mint = async () => {
    setState({ kind: "pending" });
    try {
      const hash = await mintTestTokens(account.network.chain_id, account.token.address, "1000");
      setState({ kind: "done", text: hash });
    } catch (error) {
      setState({ kind: "failed", text: errorMessage(error, "Minting failed.") });
    }
  };
  return (
    <Alert role="note" className="border-warning/50 bg-warning/10">
      <TriangleAlert aria-hidden="true" />
      <AlertTitle>Testnet demo.</AlertTitle>
      <AlertDescription className="text-foreground/80">
        <p>
          {account.network.name} and test {account.token.symbol} only; no real money moves. Test{" "}
          {account.token.symbol} is free: mint it from your wallet (gas is {account.network.name} ETH from a
          public faucet).{" "}
          <Button
            type="button"
            variant="link"
            className="h-auto p-0 text-foreground underline"
            onClick={() => void mint()}
            disabled={state.kind === "pending"}
          >
            {state.kind === "pending" ? "Confirm in your wallet…" : `Get 1,000 test ${account.token.symbol}`}
          </Button>
          <span aria-live="polite">
            {state.kind === "done" && state.text !== undefined && (
              <>
                {" "}
                Minted: <ExplorerLink account={account} kind="tx" value={state.text} />
              </>
            )}
            {state.kind === "failed" && ` ${state.text ?? ""}`}
          </span>
        </p>
      </AlertDescription>
    </Alert>
  );
}

function BalanceCard({ account }: { account: Account | null }) {
  return (
    <Card role="region" aria-labelledby="balance-title">
      <CardHeader>
        <CardDescription>
          <h2 id="balance-title">Account balance</h2>
        </CardDescription>
        <div className="text-3xl font-semibold tracking-tight tabular-nums" aria-live="polite" data-testid="balance">
          {account === null ? <Skeleton className="h-9 w-32" /> : dollars(account.balance)}
        </div>
      </CardHeader>
      <CardContent className="flex flex-col gap-3">
        <p className="text-xs text-muted-foreground">
          Workspace <code>{account?.account_id ?? "…"}</code>, a demo account kept in a cookie in this
          browser.
        </p>
        {account !== null && account.ledger.length > 0 && (
          <details className="group">
            <summary className="flex cursor-pointer list-none items-center gap-1 text-xs font-medium [&::-webkit-details-marker]:hidden">
              <ChevronRight className="size-3.5 transition-transform group-open:rotate-90" aria-hidden="true" />
              How this balance adds up ({account.ledger.length})
            </summary>
            <Table className="mt-2 text-xs">
              <TableHeader>
                <TableRow>
                  <TableHead scope="col">When</TableHead>
                  <TableHead scope="col">Deposit</TableHead>
                  <TableHead scope="col">Event</TableHead>
                  <TableHead scope="col">Amount</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {account.ledger.map((line) => (
                  <TableRow key={`${line.deposit}-${line.reason}-${line.at}`} data-testid="ledger-line">
                    <TableCell>{time(line.at)}</TableCell>
                    <TableCell className="font-mono" title={line.deposit}>
                      {short(line.deposit)}
                    </TableCell>
                    <TableCell>
                      <code>{line.reason}</code>
                    </TableCell>
                    <TableCell className={line.amount < 0 ? "text-destructive" : "text-success"}>
                      {signedDollars(line.amount)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </details>
        )}
      </CardContent>
    </Card>
  );
}

function AmountPicker({ account, onQuote }: { account: Account | null; onQuote: (created: CreatedQuote) => void }) {
  const [preset, setPreset] = useState<number | "custom">(2000);
  const [custom, setCustom] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const id = useId();
  const min = account?.min_amount ?? 100;
  const max = account?.max_amount ?? 100_000;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const cents = preset === "custom" ? Math.round(Number(custom) * 100) : preset;
    if (!Number.isSafeInteger(cents) || cents < min || cents > max) {
      setError(`Enter an amount between ${dollars(min)} and ${dollars(max)}.`);
      return;
    }
    setPending(true);
    setError(null);
    createQuote(cents).then(
      (created) => {
        setPending(false);
        onQuote(created);
      },
      (cause: unknown) => {
        setPending(false);
        setError(`Could not create the quote: ${describe(cause)}.`);
      },
    );
  };
  const options = [
    ...(account?.presets ?? [500, 2000, 5000]).map((cents) => ({ value: String(cents), label: dollars(cents) })),
    { value: "custom", label: "Custom" },
  ];

  return (
    <form onSubmit={submit} className="flex flex-col gap-4">
      <FieldSet>
        <FieldLegend variant="label" className="text-muted-foreground">
          Amount
        </FieldLegend>
        <RadioGroup
          value={String(preset)}
          onValueChange={(value) => setPreset(value === "custom" ? "custom" : Number(value))}
          className="grid-cols-2"
        >
          {options.map((option) => (
            <FieldLabel key={option.value} htmlFor={`${id}-${option.value}`}>
              <Field orientation="horizontal">
                <RadioGroupItem value={option.value} id={`${id}-${option.value}`} />
                <FieldTitle>{option.label}</FieldTitle>
              </Field>
            </FieldLabel>
          ))}
        </RadioGroup>
      </FieldSet>
      {preset === "custom" && (
        <Field>
          <FieldLabel htmlFor={`${id}-amount`}>Custom amount (USD)</FieldLabel>
          <InputGroup>
            <InputGroupAddon>
              <InputGroupText>$</InputGroupText>
            </InputGroupAddon>
            <InputGroupInput
              id={`${id}-amount`}
              inputMode="decimal"
              placeholder="25.00"
              value={custom}
              onChange={(event) => setCustom(event.target.value)}
            />
          </InputGroup>
        </Field>
      )}
      <Button type="submit" size="lg" className="w-full" disabled={pending || account === null}>
        {pending ? "Creating quote…" : "Pay with crypto"}
      </Button>
      <p className="text-xs text-muted-foreground">
        A quote locks the price for 15 minutes for an exact amount. Pay from a browser wallet, by QR
        code, or by sending the exact amount manually; another amount, or a late payment, is credited
        at the market rate instead.
      </p>
      {error !== null && (
        <Alert variant="destructive">
          <CircleAlert aria-hidden="true" />
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </form>
  );
}

function Payments({
  account,
  selected,
  onSelect,
}: {
  account: Account | null;
  selected: Selection | null;
  onSelect: (selection: Selection) => void;
}) {
  const symbol = account?.token.symbol ?? "PHA";
  return (
    <Card role="region" aria-labelledby="history-title">
      <CardHeader>
        <CardTitle>
          <h2 id="history-title">Payments</h2>
        </CardTitle>
      </CardHeader>
      <CardContent>
        {account === null || account.payments.length === 0 ? (
          <p className="text-muted-foreground">No top-ups yet.</p>
        ) : (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead scope="col">Date</TableHead>
                <TableHead scope="col">Method</TableHead>
                <TableHead scope="col">{symbol}</TableHead>
                <TableHead scope="col">Transaction</TableHead>
                <TableHead scope="col">Status</TableHead>
                <TableHead scope="col">Credited</TableHead>
                <TableHead scope="col">Refunded</TableHead>
                <TableHead scope="col">Nets to</TableHead>
                <TableHead scope="col">
                  <span className="sr-only">Timeline</span>
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {account.payments.map((row) => {
                const selection: Selection = row.id.startsWith("dep_")
                  ? { kind: "deposit", id: row.id }
                  : { kind: "quote", id: row.id };
                const isSelected = selected?.id === row.id || selected?.id === row.quote;
                return (
                  <TableRow
                    key={row.id}
                    data-testid="payment"
                    data-kind={row.kind}
                    data-state={isSelected ? "selected" : undefined}
                    aria-selected={isSelected}
                  >
                    <TableCell>{time(row.created)}</TableCell>
                    <TableCell>{row.kind === "quote" ? "Quote" : "Deposit address"}</TableCell>
                    <TableCell>{tokens(row.amount_atomic, symbol)}</TableCell>
                    <TableCell>
                      {row.tx_hash === null ? "—" : <ExplorerLink account={account} kind="tx" value={row.tx_hash} />}
                    </TableCell>
                    <TableCell>
                      <div className="flex gap-1">
                        <StatusBadge status={row.status}>{statusLabel(row.status)}</StatusBadge>
                        {row.final && <Badge variant="outline">final</Badge>}
                        {row.swept && <StatusBadge status="swept">swept</StatusBadge>}
                      </div>
                    </TableCell>
                    <TableCell>{row.amount === null || row.tx_hash === null ? "—" : dollars(row.amount)}</TableCell>
                    <TableCell>
                      {row.amount_refunded_atomic === "0" ? "—" : tokens(row.amount_refunded_atomic, symbol)}
                    </TableCell>
                    <TableCell>{row.net === null ? "—" : dollars(row.net)}</TableCell>
                    <TableCell className="text-right">
                      <Button
                        type="button"
                        variant="link"
                        className="h-auto p-0"
                        onClick={() => onSelect(selection)}
                        aria-label={`Timeline of ${row.id}`}
                      >
                        Timeline
                      </Button>
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        )}
      </CardContent>
    </Card>
  );
}

function TrustStrip({ trust, account }: { trust: Trust | null; account: Account | null }) {
  const attestation = trust?.attestation;
  const evidence = trust?.tls_evidence;
  return (
    <Card role="region" aria-labelledby="trust-title">
      <CardHeader>
        <CardTitle>
          <h2 id="trust-title">Why you can trust Phala Pay</h2>
        </CardTitle>
      </CardHeader>
      <CardContent className="grid gap-6 text-xs md:grid-cols-3">
        <div className="flex flex-col gap-2">
          <h3 className="flex items-center gap-2 text-sm font-medium">
            <ShieldCheck className="size-4 text-muted-foreground" aria-hidden="true" />
            Attestation
          </h3>
          {attestation === undefined ? (
            <p className="text-muted-foreground">Loading…</p>
          ) : attestation.binding_verified ? (
            <p>
              <span className="font-medium text-success">Verified</span> for a fresh nonce: the TDX quote's
              report data binds this account's webhook key{" "}
              <code title={attestation.webhook_public_key}>{short(attestation.webhook_public_key ?? "")}</code> that
              signs every webhook ({attestation.quote_bytes ?? 0}-byte quote).
            </p>
          ) : (
            <p className="text-destructive">The attestation did not bind its keys.</p>
          )}
        </div>
        <div className="flex flex-col gap-2">
          <h3 className="flex items-center gap-2 text-sm font-medium">
            <Cpu className="size-4 text-muted-foreground" aria-hidden="true" />
            Application
          </h3>
          {evidence == null ? (
            <p className="text-muted-foreground">TLS evidence unavailable.</p>
          ) : (
            <Details>
              <Detail label="App id" className="font-mono">
                {evidence.app_id}
              </Detail>
              {evidence.compose_hash !== undefined && (
                <Detail label="Compose hash" className="font-mono" title={evidence.compose_hash}>
                  {short(evidence.compose_hash)}
                </Detail>
              )}
            </Details>
          )}
          <p className="text-muted-foreground">From the TLS certificate evidence quote (at issuance).</p>
        </div>
        <div className="flex flex-col gap-2">
          <h3 className="flex items-center gap-2 text-sm font-medium">
            <Wallet className="size-4 text-muted-foreground" aria-hidden="true" />
            Non-custodial
          </h3>
          <p>
            Every address pays only the merchant's treasury, fixed in the address. Phala Pay holds no
            funds and sends no transactions: the merchant sweeps and refunds itself.
          </p>
          <p>
            <a className={LINK} href={trust?.verify_docs} target="_blank" rel="noreferrer">
              Attestation guide
            </a>{" "}
            ·{" "}
            <a className={LINK} href={trust?.dstack_verifier} target="_blank" rel="noreferrer">
              dstack verifier
            </a>
          </p>
          <p className="text-muted-foreground">
            Network: {account?.network.name ?? "Sepolia"} {account?.network.testnet === false ? "" : "testnet"}
          </p>
        </div>
      </CardContent>
    </Card>
  );
}
