import { formatCountdown } from "@phala/pay";
import type { Appearance } from "@phala/pay/react";
import { useMutation } from "@tanstack/react-query";
import { AppWindow, ArrowRight, CircleAlert, FlaskConical, Lock } from "lucide-react";
import { Suspense, lazy, useEffect, useId, useState, type FormEvent, type ReactNode } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field, FieldContent, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupInput, InputGroupText } from "@/components/ui/input-group";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import type { Account, Asset, CreatedQuote, DepositAddressResponse } from "./api.js";
import { BRAND_BUTTON, ExplorerLink, InfoTip, LINK, describe, errorMessage, loadSdk, wallet } from "./common.js";
import { DepositAddressPanel } from "./DepositAddressPanel.js";
import { dollars, price, tokenName } from "./format.js";
import { useCreateQuote } from "./queries.js";

const Checkout = lazy(() => loadSdk().then((sdk) => ({ default: sdk.Checkout })));

// ethereum.org's list of Sepolia faucets: gas for the visitor's own wallet.
const SEPOLIA_FAUCETS = "https://ethereum.org/en/developers/docs/networks/#sepolia";

export type Method = "quote" | "address";

const METHODS: { id: Method; label: string }[] = [
  { id: "quote", label: "Exact amount" },
  { id: "address", label: "Deposit address" },
];

/**
 * The product: the cloud console's billing page, as its customer sees it, with Phala Pay inside.
 * Only customer-facing UI belongs here; what the backend sees is in ./Backend.
 */
export function Product({
  account,
  accountError,
  assets,
  method,
  onMethodChange,
  session,
  onQuote,
  onNewTopUp,
  onCredited,
  address,
  onAddress,
  appearance,
}: {
  account: Account | null;
  accountError: string | null;
  assets: Asset[] | undefined;
  method: Method;
  onMethodChange: (method: Method) => void;
  session: CreatedQuote | null;
  onQuote: (created: CreatedQuote) => void;
  onNewTopUp: () => void;
  onCredited: () => void;
  address: DepositAddressResponse | null;
  onAddress: (created: DepositAddressResponse) => void;
  appearance: Appearance;
}) {
  // The token the customer pays with, for either method; the first offered until they choose.
  const [choice, setChoice] = useState<string | null>(null);
  const asset = assets?.find((each) => each.asset === choice) ?? assets?.[0];
  const testnet = account?.network.testnet ?? true;
  return (
    <div className="flex min-w-0 flex-col gap-3 lg:sticky lg:top-20">
      <AreaLabel icon={<AppWindow />} title="Your product" text="What your customer sees" />
      <section
        aria-labelledby="product-title"
        className="product-app overflow-hidden rounded-2xl border bg-card text-card-foreground shadow-[0_1px_2px_rgb(0_0_0/0.04),0_12px_40px_-12px_rgb(0_0_0/0.12)] dark:shadow-[0_1px_0_rgb(255_255_255/0.06)_inset,0_16px_48px_-16px_rgb(0_0_0/0.7)]"
      >
        <div className="grid h-10 grid-cols-[4.75rem_minmax(0,1fr)_4.75rem] items-center gap-2 border-b bg-muted/50 px-3.5">
          <span className="flex gap-1.5" aria-hidden="true">
            <span className="size-2.5 rounded-full bg-foreground/12" />
            <span className="size-2.5 rounded-full bg-foreground/12" />
            <span className="size-2.5 rounded-full bg-foreground/12" />
          </span>
          <span
            className="mx-auto flex min-w-0 items-center gap-1.5 rounded-md bg-background px-3 py-1 text-[0.6875rem] text-muted-foreground ring-1 ring-border"
            aria-hidden="true"
          >
            <Lock className="size-2.5 shrink-0" />
            <span className="truncate">Cloud Console · Billing</span>
          </span>
          {testnet && <TestnetBadge network={account?.network.name ?? "Sepolia"} />}
        </div>
        <div className="flex flex-col gap-7 p-5 sm:p-7">
          <div className="flex items-start justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h2 id="product-title" className="sr-only">
                Cloud Console · Billing
              </h2>
              <h3 id="balance-title" className="text-[0.8125rem] text-muted-foreground">
                Account balance
              </h3>
              <div
                className="text-4xl font-semibold tracking-[-0.03em] tabular-nums"
                aria-live="polite"
                aria-labelledby="balance-title"
                data-testid="balance"
              >
                {account === null ? <Skeleton className="h-10 w-36" /> : dollars(account.balance)}
              </div>
            </div>
            <Workspace account={account} />
          </div>
          {accountError !== null && (
            <Alert variant="destructive">
              <CircleAlert aria-hidden="true" />
              <AlertDescription>Could not load the account: {accountError}</AlertDescription>
            </Alert>
          )}
          <section aria-labelledby="pay-title" className="flex flex-col gap-4">
            <h3 id="pay-title" className="text-[0.9375rem] font-medium">
              Add credits
            </h3>
            <Tabs value={method} onValueChange={(value) => onMethodChange(value === "address" ? "address" : "quote")}>
              <TabsList aria-label="Payment method" className="h-9! w-full">
                {METHODS.map(({ id, label }) => (
                  <TabsTrigger key={id} value={id}>
                    {label}
                  </TabsTrigger>
                ))}
              </TabsList>
              <TabsContent value="quote" className="pt-5">
                {session === null || account === null ? (
                  <AmountPicker account={account} assets={assets} asset={asset} onAssetChange={setChoice} onQuote={onQuote} />
                ) : (
                  <div className="flex flex-col gap-5">
                    <LockedRate session={session} testnet={testnet} />
                    <Suspense fallback={<CheckoutSkeleton />}>
                      <Checkout
                        clientSecret={session.client_secret}
                        expectedAddress={session.expected_address}
                        apiBase={account.api_base}
                        appearance={appearance}
                        onSuccess={onCredited}
                      />
                    </Suspense>
                    <Button type="button" variant="ghost" className="self-center text-muted-foreground" onClick={onNewTopUp}>
                      Start a new top-up
                    </Button>
                  </div>
                )}
              </TabsContent>
              <TabsContent value="address" className="pt-5">
                {account === null ? (
                  <CheckoutSkeleton />
                ) : (
                  <DepositAddressPanel
                    account={account}
                    picker={<TokenPicker assets={assets} asset={asset} onChange={setChoice} hint="Market rate" />}
                    asset={asset}
                    appearance={appearance}
                    created={address}
                    onCreated={onAddress}
                  />
                )}
              </TabsContent>
            </Tabs>
          </section>
        </div>
      </section>
      {testnet && account !== null && <TestTokens account={account} />}
    </div>
  );
}

/** A small caption above each of the page's two areas. */
export function AreaLabel({ icon, title, text }: { icon: ReactNode; title: string; text: string }) {
  return (
    <p className="flex h-5 items-center gap-2 px-1 text-xs [&_svg]:size-3.5 [&_svg]:text-muted-foreground">
      {icon}
      <span className="font-medium">{title}</span>
      <span className="text-muted-foreground">· {text}</span>
    </p>
  );
}

/** The product frame's testnet marker: in its title bar, where a customer looks for where they are. */
function TestnetBadge({ network }: { network: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          data-testid="testnet-badge"
          className="flex items-center gap-1 justify-self-end rounded-full bg-amber-500/15 px-2 py-0.5 text-[0.6875rem] font-semibold text-amber-800 ring-1 ring-amber-600/25 outline-none ring-inset focus-visible:ring-2 focus-visible:ring-ring dark:text-amber-300 dark:ring-amber-400/25"
        >
          <FlaskConical className="size-3" aria-hidden="true" />
          Testnet
        </button>
      </TooltipTrigger>
      <TooltipContent className="max-w-xs leading-relaxed">
        A demo on the {network} testnet: you pay with free test tokens, and no real money moves.
      </TooltipContent>
    </Tooltip>
  );
}

function Workspace({ account }: { account: Account | null }) {
  if (account === null) {
    return <Skeleton className="h-6 w-28 rounded-full" />;
  }
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={`Workspace ${account.account_id}`}
          className="flex max-w-36 items-center gap-1.5 rounded-full border px-2.5 py-1 font-mono text-[0.6875rem] text-muted-foreground outline-none focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span className="size-1.5 shrink-0 rounded-full bg-success" aria-hidden="true" />
          <span className="truncate">{account.account_id}</span>
        </button>
      </TooltipTrigger>
      <TooltipContent>
        Workspace <span className="font-mono">{account.account_id}</span>, a demo account kept in a cookie in this
        browser.
      </TooltipContent>
    </Tooltip>
  );
}

function CheckoutSkeleton() {
  return (
    <div className="flex flex-col gap-3" aria-hidden="true">
      <Skeleton className="h-7 w-40" />
      <Skeleton className="h-4 w-56" />
      <Skeleton className="h-10 w-full" />
      <Skeleton className="h-10 w-full" />
    </div>
  );
}

/**
 * The tokens the service accepts on the product's chain, as radio cards: shown even with one, so
 * the customer sees what they pay with (a test token, on a testnet) before paying.
 */
function TokenPicker({
  assets,
  asset,
  onChange,
  hint,
}: {
  assets: Asset[] | undefined;
  asset: Asset | undefined;
  onChange: (asset: string) => void;
  hint: string;
}) {
  const id = useId();
  return (
    <FieldSet className="gap-2!">
      <FieldLegend variant="label" className="mb-0 text-[0.8125rem] text-muted-foreground">
        Pay with
      </FieldLegend>
      {assets === undefined ? (
        <Skeleton className="h-[3.625rem] w-full rounded-lg" />
      ) : assets.length === 0 ? (
        <p className="text-sm text-muted-foreground">No token is accepted right now.</p>
      ) : (
        <RadioGroup value={asset?.asset ?? ""} onValueChange={onChange} aria-label="Token" className="gap-2">
          {assets.map((each) => (
            <FieldLabel
              key={`${each.chain_id}:${each.asset}`}
              htmlFor={`${id}-${each.asset}`}
              data-testid="token-option"
              className={CHOICE}
            >
              <Field orientation="horizontal" className="items-center! gap-3 px-3.5! py-2.5!">
                <RadioGroupItem value={each.asset} id={`${id}-${each.asset}`} />
                <TokenMark symbol={each.symbol} />
                <FieldContent className="min-w-0 gap-0">
                  <span className="text-sm font-medium">{tokenName(each.symbol, each.testnet)}</span>
                  <span className="text-xs text-muted-foreground">
                    {each.network}
                    {each.testnet && " testnet"}
                  </span>
                </FieldContent>
                <span className="shrink-0 text-xs text-muted-foreground">{hint}</span>
              </Field>
            </FieldLabel>
          ))}
        </RadioGroup>
      )}
    </FieldSet>
  );
}

/** A choice card's selected state: a quiet outline, not the primary fill. */
const CHOICE =
  "cursor-pointer transition-colors has-data-checked:border-foreground/70! has-data-checked:bg-transparent! has-data-checked:ring-1 has-data-checked:ring-foreground/70 dark:has-data-checked:bg-transparent!";

function TokenMark({ symbol }: { symbol: string }) {
  return (
    <span
      className="flex size-7 shrink-0 items-center justify-center rounded-full bg-foreground text-[0.5625rem] font-bold tracking-tight text-background"
      aria-hidden="true"
    >
      {symbol.slice(0, 3)}
    </span>
  );
}

function AmountPicker({
  account,
  assets,
  asset,
  onAssetChange,
  onQuote,
}: {
  account: Account | null;
  assets: Asset[] | undefined;
  asset: Asset | undefined;
  onAssetChange: (asset: string) => void;
  onQuote: (created: CreatedQuote) => void;
}) {
  const [preset, setPreset] = useState<number | "custom">(2000);
  const [custom, setCustom] = useState("");
  const [invalid, setInvalid] = useState<string | null>(null);
  const quote = useCreateQuote();
  const id = useId();
  const min = Math.max(account?.min_amount ?? 100, asset?.min_amount ?? 0);
  const max = account?.max_amount ?? 100_000;

  const submit = (event: FormEvent) => {
    event.preventDefault();
    const cents = preset === "custom" ? Math.round(Number(custom) * 100) : preset;
    if (!Number.isSafeInteger(cents) || cents < min || cents > max) {
      setInvalid(`Enter an amount between ${dollars(min)} and ${dollars(max)}.`);
      return;
    }
    if (asset === undefined) {
      return;
    }
    setInvalid(null);
    // The checkout's code loads while the quote is created.
    loadSdk().catch(() => undefined);
    quote.mutate({ amount: cents, asset: asset.asset }, { onSuccess: onQuote });
  };
  const options = [
    ...(account?.presets ?? [500, 2000, 5000]).map((cents) => ({ value: String(cents), label: dollars(cents) })),
    { value: "custom", label: "Custom" },
  ];
  const error = invalid ?? (quote.error === null ? null : `Could not create the quote: ${describe(quote.error)}.`);
  const minutes = Math.round((asset?.quote_ttl_seconds ?? 900) / 60);

  return (
    <form onSubmit={submit} className="flex flex-col gap-5">
      <FieldSet className="gap-2!">
        <FieldLegend variant="label" className="mb-0 text-[0.8125rem] text-muted-foreground">
          Amount
        </FieldLegend>
        <RadioGroup
          value={String(preset)}
          onValueChange={(value) => setPreset(value === "custom" ? "custom" : Number(value))}
          aria-label="Amount"
          className="grid-cols-2 gap-2 sm:grid-cols-4"
        >
          {options.map((option) => (
            <FieldLabel key={option.value} htmlFor={`${id}-${option.value}`} className={cn(CHOICE, "relative")}>
              <RadioGroupItem
                value={option.value}
                id={`${id}-${option.value}`}
                className="pointer-events-none absolute! opacity-0"
              />
              <Field orientation="horizontal" className="justify-center py-3!">
                <span className="text-sm font-medium tabular-nums">{option.label}</span>
              </Field>
            </FieldLabel>
          ))}
        </RadioGroup>
      </FieldSet>
      {preset === "custom" && (
        <Field>
          <FieldLabel htmlFor={`${id}-amount`}>Custom amount (USD)</FieldLabel>
          <InputGroup className="h-10">
            <InputGroupAddon>
              <InputGroupText>$</InputGroupText>
            </InputGroupAddon>
            <InputGroupInput
              id={`${id}-amount`}
              inputMode="decimal"
              placeholder="25.00"
              className="tabular-nums"
              value={custom}
              onChange={(event) => setCustom(event.target.value)}
            />
          </InputGroup>
        </Field>
      )}
      <TokenPicker assets={assets} asset={asset} onChange={onAssetChange} hint="Rate locked at checkout" />
      <div className="flex flex-col gap-3">
        <Button
          type="submit"
          size="lg"
          className={BRAND_BUTTON}
          disabled={quote.isPending || account === null || asset === undefined}
        >
          {quote.isPending ? "Creating quote…" : "Pay with crypto"}
        </Button>
        <p className="flex items-center justify-center gap-1.5 text-xs text-muted-foreground">
          Price locked for {minutes} minutes
          <InfoTip label="About paying for an exact amount">
            A quote locks the price for {minutes} minutes for an exact amount. Pay from a browser wallet, by QR code,
            or by sending the exact amount manually; another amount, or a late payment, is credited at the market rate
            instead.
          </InfoTip>
        </p>
      </div>
      {error !== null && (
        <Alert variant="destructive">
          <CircleAlert aria-hidden="true" />
          <AlertDescription>{error}</AlertDescription>
        </Alert>
      )}
    </form>
  );
}

/** The quote's locked price, from `quote.exchange_rate`, and how long it holds. */
function LockedRate({ session, testnet }: { session: CreatedQuote; testnet: boolean }) {
  const now = useNow();
  const symbol = session.asset.toUpperCase();
  const expired = now >= session.expires_at * 1000;
  return (
    <div
      data-testid="locked-rate"
      className="flex items-center justify-between gap-4 rounded-lg border bg-muted/40 px-4 py-3"
    >
      <div className="flex min-w-0 flex-col gap-0.5">
        <span className="text-xs text-muted-foreground">Locked rate · {tokenName(symbol, testnet)}</span>
        <span className="text-[0.9375rem] font-semibold tabular-nums">
          1 {symbol} = {price(session.exchange_rate)}
        </span>
      </div>
      <div className="flex shrink-0 flex-col items-end gap-0.5">
        <span className="text-xs text-muted-foreground">{expired ? "Expired" : "Locked for"}</span>
        <span className={cn("font-mono text-[0.9375rem] font-medium tabular-nums", expired && "text-destructive")}>
          {formatCountdown(session.expires_at, now)}
        </span>
      </div>
    </div>
  );
}

/** The current time in milliseconds, updated every second: a countdown's clock. */
function useNow(): number {
  const [now, setNow] = useState(Date.now);
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, []);
  return now;
}

/** Where to get test tokens: the test token's public mint, from the visitor's wallet, and gas. */
function TestTokens({ account }: { account: Account }) {
  const symbol = account.token.symbol;
  const mint = useMutation({
    mutationFn: async () => {
      const { mintTestTokens } = await wallet();
      return mintTestTokens(account.network.chain_id, account.token.address, "1000");
    },
  });
  return (
    <div role="note" aria-label="Test tokens" className="flex flex-col gap-1 px-1 text-xs text-muted-foreground">
      <p className="flex flex-wrap items-center gap-x-1.5 gap-y-1">
        <span>Need test tokens?</span>
        <button
          type="button"
          className="group/mint inline-flex items-center gap-1 rounded-sm font-medium text-foreground outline-none hover:underline hover:underline-offset-4 focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-60"
          onClick={() => mint.mutate()}
          disabled={mint.isPending}
        >
          {mint.isPending ? "Confirm in your wallet…" : `Mint 1,000 test ${symbol}`}
          <ArrowRight
            className="size-3 transition-transform group-hover/mint:translate-x-0.5 motion-reduce:transition-none"
            aria-hidden="true"
          />
        </button>
        <span className="flex items-center gap-1.5 whitespace-nowrap">
          <span className="max-sm:hidden" aria-hidden="true">
            ·
          </span>
          <a className={LINK} href={SEPOLIA_FAUCETS} target="_blank" rel="noreferrer">
            {account.network.name} ETH faucet
          </a>
          <InfoTip label="About test tokens" className="translate-y-0">
            Test {symbol} is free: its contract lets anyone mint it, so your own wallet mints it. Gas is{" "}
            {account.network.name} ETH, also free, from a public faucet.
          </InfoTip>
        </span>
      </p>
      <p aria-live="polite" className="empty:hidden">
        {mint.isSuccess && (
          <>
            Minted: <ExplorerLink account={account} kind="tx" value={mint.data} />
          </>
        )}
        {mint.isError && <span className="text-destructive">{errorMessage(mint.error, "Minting failed.")}</span>}
      </p>
    </div>
  );
}
