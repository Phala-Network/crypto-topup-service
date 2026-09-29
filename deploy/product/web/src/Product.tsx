import type { CheckoutStatus } from "@phala/pay";
import type { Appearance } from "@phala/pay/react";
import { useMutation } from "@tanstack/react-query";
import { AppWindow, Check, CircleAlert, CircleCheck, Copy, ExternalLink, FlaskConical, Gift, Lock } from "lucide-react";
import { Suspense, lazy, useId, useState, type FormEvent, type ReactNode } from "react";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupInput, InputGroupText } from "@/components/ui/input-group";
import { Label } from "@/components/ui/label";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import type { Account, Asset, CreatedQuote, DepositAddressResponse, Network } from "./api.js";
import { ChainIcon, TokenIcon, assetOf, networkOf, tokenFullName } from "./chains.js";
import { BRAND_BUTTON, ExplorerLink, InfoTip, describe, errorMessage, loadSdk, wallet } from "./common.js";
import { DepositAddressPanel } from "./DepositAddressPanel.js";
import { dollars, percent, rate, signedDollars, tokenName } from "./format.js";
import { useCreateQuote } from "./queries.js";

const Checkout = lazy(() => loadSdk().then((sdk) => ({ default: sdk.Checkout })));

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
  networks,
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
  networks: Network[] | undefined;
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
  // The network, then the token, the customer pays with, for either method: the first offered
  // until they choose; a network's first token when they change the network.
  const [choice, setChoice] = useState<{ chainId: number | null; asset: string | null }>({
    chainId: null,
    asset: null,
  });
  const network = networks?.find((each) => each.chain_id === choice.chainId) ?? networks?.[0];
  const asset = network?.assets.find((each) => each.asset === choice.asset) ?? network?.assets[0];
  const picker = (
    <PaymentOptions
      networks={networks}
      network={network}
      asset={asset}
      onNetworkChange={(chainId) => setChoice({ chainId, asset: null })}
      onAssetChange={(value) => setChoice({ chainId: network?.chain_id ?? null, asset: value })}
    />
  );
  return (
    <div className="flex min-w-0 flex-col gap-3 lg:sticky lg:top-20">
      <AreaLabel icon={<AppWindow />} title="Your product" text="What your customer sees" />
      <section
        aria-labelledby="product-title"
        className="product-app overflow-hidden rounded-xl border bg-card text-card-foreground shadow-sm"
      >
        <div className="flex h-10 items-center gap-3 border-b bg-muted/40 px-4">
          <span className="flex gap-1.5" aria-hidden="true">
            <span className="size-2.5 rounded-full bg-foreground/15" />
            <span className="size-2.5 rounded-full bg-foreground/15" />
            <span className="size-2.5 rounded-full bg-foreground/15" />
          </span>
          <span className="flex min-w-0 flex-1 items-center justify-center gap-1.5 text-xs text-muted-foreground" aria-hidden="true">
            <Lock className="size-3 shrink-0" />
            <span className="truncate">Cloud Console · Billing</span>
          </span>
          {(network?.testnet ?? true) && <TestnetBadge network={network?.name ?? "a"} />}
        </div>
        <div className="flex flex-col gap-6 p-5 sm:p-6">
          <div className="flex items-start justify-between gap-4">
            <div className="flex flex-col gap-1">
              <h2 id="product-title" className="sr-only">
                Cloud Console · Billing
              </h2>
              <h3 id="balance-title" className="text-sm text-muted-foreground">
                Account balance
              </h3>
              <div
                className="text-3xl font-semibold tracking-tight tabular-nums"
                aria-live="polite"
                aria-labelledby="balance-title"
                data-testid="balance"
              >
                {account === null ? <Skeleton className="h-9 w-32" /> : dollars(account.balance)}
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
          <Separator />
          <section aria-labelledby="pay-title" className="flex flex-col gap-4">
            <h3 id="pay-title" className="text-base font-semibold">
              Add credits
            </h3>
            <Tabs value={method} onValueChange={(value) => onMethodChange(value === "address" ? "address" : "quote")}>
              <TabsList aria-label="Payment method" className="w-full">
                {METHODS.map(({ id, label }) => (
                  <TabsTrigger key={id} value={id}>
                    {label}
                  </TabsTrigger>
                ))}
              </TabsList>
              <TabsContent value="quote" className="pt-4">
                {session === null || account === null ? (
                  <AmountPicker account={account} network={network} asset={asset} picker={picker} onQuote={onQuote} />
                ) : (
                  <QuoteCheckout
                    key={session.quote}
                    session={session}
                    account={account}
                    network={networkOf(networks, session.chain_id)}
                    appearance={appearance}
                    onCredited={onCredited}
                    onNewTopUp={onNewTopUp}
                  />
                )}
              </TabsContent>
              <TabsContent value="address" className="pt-4">
                {account === null ? (
                  <CheckoutSkeleton />
                ) : (
                  <DepositAddressPanel
                    account={account}
                    picker={picker}
                    network={network}
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
      {network?.testnet === true && asset !== undefined && <TestTokens network={network} asset={asset} />}
    </div>
  );
}

/** A small caption above each of the page's two areas. */
export function AreaLabel({ icon, title, text }: { icon: ReactNode; title: string; text: string }) {
  return (
    <p className="flex h-5 items-center gap-2 px-1 text-[0.8125rem] [&_svg]:size-4 [&_svg]:text-muted-foreground">
      {icon}
      <span>
        <span className="font-medium">{title}</span>
        <span className="text-muted-foreground"> · {text}</span>
      </span>
    </p>
  );
}

/** The product frame's testnet marker: in its title bar, where a customer looks for where they are. */
function TestnetBadge({ network }: { network: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button type="button" data-testid="testnet-badge" className="rounded-full outline-none focus-visible:ring-2 focus-visible:ring-ring">
          <TestnetTag />
        </button>
      </TooltipTrigger>
      <TooltipContent>A demo on {network}: you pay with free test tokens, and no real money moves.</TooltipContent>
    </Tooltip>
  );
}

/** The testnet marker, the same in the title bar and the network select. */
function TestnetTag() {
  return (
    <Badge variant="outline" className="gap-1 border-amber-500/40 bg-amber-500/10 text-amber-700 dark:text-amber-300">
      <FlaskConical aria-hidden="true" />
      Testnet
    </Badge>
  );
}

/** The demo workspace: its id, truncated, with a copy button. */
function Workspace({ account }: { account: Account | null }) {
  const [copied, setCopied] = useState(false);
  if (account === null) {
    return <Skeleton className="h-7 w-32 rounded-full" />;
  }
  const copy = () => {
    navigator.clipboard.writeText(account.account_id).then(
      () => {
        setCopied(true);
        setTimeout(() => setCopied(false), 1500);
      },
      () => undefined,
    );
  };
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          onClick={copy}
          aria-label={`Workspace ${account.account_id}, copy`}
          className="flex h-7 max-w-40 items-center gap-1.5 rounded-full border px-2.5 font-mono text-xs text-muted-foreground transition-colors outline-none hover:bg-muted focus-visible:ring-2 focus-visible:ring-ring"
        >
          <span className="size-1.5 shrink-0 rounded-full bg-success" aria-hidden="true" />
          <span className="truncate">{account.account_id}</span>
          {copied ? <Check className="size-3 shrink-0" aria-hidden="true" /> : <Copy className="size-3 shrink-0" aria-hidden="true" />}
        </button>
      </TooltipTrigger>
      <TooltipContent>{copied ? "Copied" : "Your demo account, kept in this browser's cookie."}</TooltipContent>
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
 * The network, then the token, as checkouts and wallets ask for them: a network select, and the
 * network's tokens as a list with each one's price terms. Shown even with one option each, so
 * the customer sees what they pay with (a test token, on a testnet) before paying.
 */
function PaymentOptions({
  networks,
  network,
  asset,
  onNetworkChange,
  onAssetChange,
}: {
  networks: Network[] | undefined;
  network: Network | undefined;
  asset: Asset | undefined;
  onNetworkChange: (chainId: number) => void;
  onAssetChange: (asset: string) => void;
}) {
  const id = useId();
  if (networks === undefined) {
    return (
      <div className="space-y-6" aria-hidden="true">
        <Skeleton className="h-10 w-full rounded-lg" />
        <Skeleton className="h-16 w-full rounded-lg" />
      </div>
    );
  }
  if (network === undefined || asset === undefined) {
    return <p className="text-sm text-muted-foreground">No network accepts payments right now.</p>;
  }
  return (
    <>
      <div className="space-y-2">
        <Label htmlFor={`${id}-network`}>Network</Label>
        {/* In a form, Radix adds a hidden native select beside the trigger: kept out of the flow. */}
        <div className="relative [&>select]:absolute">
          <Select value={String(network.chain_id)} onValueChange={(value) => onNetworkChange(Number(value))}>
            <SelectTrigger id={`${id}-network`} className="h-10! w-full" aria-label="Network" data-testid="network-select">
              <SelectValue />
            </SelectTrigger>
            <SelectContent position="popper" align="start">
              {networks.map((each) => (
                <SelectItem key={each.chain_id} value={String(each.chain_id)} data-testid="network-option">
                  <ChainIcon chainId={each.chain_id} />
                  <span>{each.name.replace(/ testnet$/, "")}</span>
                  {each.testnet && <TestnetTag />}
                </SelectItem>
              ))}
            </SelectContent>
          </Select>
        </div>
      </div>
      <div className="space-y-2">
        <Label id={`${id}-token`} asChild>
          <span>Token</span>
        </Label>
        {/* Keyed by network: each network lists its own tokens. */}
        <RadioGroup
          key={network.chain_id}
          value={asset.asset}
          onValueChange={onAssetChange}
          aria-label="Token"
          className="@container gap-2"
        >
          {network.assets.map((each) => (
            <TokenOption key={each.asset} id={`${id}-token-${network.chain_id}-${each.asset}`} asset={each} testnet={network.testnet} />
          ))}
        </RadioGroup>
      </div>
    </>
  );
}

/**
 * A token row: its mark, symbol, and name; on the right the demo merchant's bonus, if any, and its
 * price terms; checked, a tick. In a narrow list the bonus moves under the name.
 */
function TokenOption({ id, asset, testnet }: { id: string; asset: Asset; testnet: boolean }) {
  const bonus =
    asset.bonus_bps > 0 ? (
      <Badge className="shrink-0 bg-success/12 text-success" data-testid="token-bonus">
        <Gift aria-hidden="true" />+{percent(asset.bonus_bps)} bonus
      </Badge>
    ) : null;
  return (
    <FieldLabel
      htmlFor={id}
      data-testid="token-option"
      className="w-full min-w-0 cursor-pointer has-data-checked:border-primary! dark:has-data-checked:border-primary/60!"
    >
      <Field orientation="horizontal" className="min-w-0 items-center! gap-3 px-3! py-2.5!">
        {/* The row is the control: the radio itself stays for the keyboard and screen readers. */}
        <RadioGroupItem
          value={asset.asset}
          id={id}
          className="peer pointer-events-none absolute! opacity-0"
          aria-label={tokenName(asset.symbol, testnet)}
        />
        <TokenIcon asset={asset.asset} />
        <span className="flex min-w-0 flex-1 flex-col items-start gap-0.5">
          <span className="text-sm font-medium">{asset.symbol}</span>
          <span className="w-full truncate text-xs font-normal text-muted-foreground">
            {testnet ? `Test ${tokenFullName(asset.asset)}` : tokenFullName(asset.asset)}
          </span>
          {bonus !== null && <span className="@md:hidden">{bonus}</span>}
        </span>
        {bonus !== null && <span className="hidden @md:inline-flex">{bonus}</span>}
        {/* A stablecoin is valued at $1.00; any other token's rate is known only once a quote
            locks it (the locked-rate line). */}
        <span className="shrink-0 text-sm font-normal tabular-nums" data-testid="token-price">
          {asset.pricing === "stablecoin" ? (
            "$1.00"
          ) : (
            <span className="text-xs text-muted-foreground">Price locked at checkout</span>
          )}
        </span>
        <span
          className="flex size-4 shrink-0 items-center justify-center rounded-full bg-primary text-primary-foreground opacity-0 peer-data-checked:opacity-100"
          aria-hidden="true"
        >
          <Check className="size-3" />
        </span>
      </Field>
    </FieldLabel>
  );
}

function AmountPicker({
  account,
  network,
  asset,
  picker,
  onQuote,
}: {
  account: Account | null;
  network: Network | undefined;
  asset: Asset | undefined;
  /** The network and token choice. */
  picker: ReactNode;
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
    if (network === undefined || asset === undefined) {
      return;
    }
    setInvalid(null);
    // The checkout's code loads while the quote is created.
    loadSdk().catch(() => undefined);
    quote.mutate({ amount: cents, chainId: network.chain_id, asset: asset.asset }, { onSuccess: onQuote });
  };
  const options = [
    ...(account?.presets ?? [500, 2000, 5000]).map((cents) => ({ value: String(cents), label: dollars(cents) })),
    { value: "custom", label: "Custom" },
  ];
  const error = invalid ?? (quote.error === null ? null : `Could not create the quote: ${describe(quote.error)}.`);
  const minutes = Math.round((asset?.quote_ttl_seconds ?? 900) / 60);

  return (
    <form onSubmit={submit} className="flex flex-col gap-6">
      <div className="space-y-2">
        <Label id={`${id}-amount-label`} asChild>
          <span>Amount</span>
        </Label>
        <RadioGroup
          value={String(preset)}
          onValueChange={(value) => setPreset(value === "custom" ? "custom" : Number(value))}
          aria-labelledby={`${id}-amount-label`}
          className="grid-cols-4 gap-2"
        >
          {options.map((option) => (
            <FieldLabel
              key={option.value}
              htmlFor={`${id}-${option.value}`}
              className="relative cursor-pointer has-data-checked:border-primary! dark:has-data-checked:border-primary/60!"
            >
              <RadioGroupItem
                value={option.value}
                id={`${id}-${option.value}`}
                className="pointer-events-none absolute! opacity-0"
              />
              <Field orientation="horizontal" className="h-10 justify-center px-1! py-0!">
                <span className="text-sm font-medium tabular-nums">{option.label}</span>
              </Field>
            </FieldLabel>
          ))}
        </RadioGroup>
      </div>
      {preset === "custom" && (
        <div className="space-y-2">
          <Label htmlFor={`${id}-amount`}>Custom amount (USD)</Label>
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
        </div>
      )}
      {picker}
      <div className="flex flex-col gap-2.5">
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
            A quote locks the price for {minutes} minutes for an exact amount. Pay from a browser wallet, by QR code, or
            by sending the exact amount manually; another amount, or a late payment, is credited at the market rate
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

// While a quote can still be paid at its locked price; the SDK's status line says what happened
// after (expired, …) and holds the one countdown.
const PAYABLE: ReadonlySet<CheckoutStatus> = new Set(["loading", "waiting", "seen", "confirming"]);

/**
 * The quote's checkout: the SDK's `<Checkout>`, with the locked rate above it while it applies;
 * once credited, one confirmation of what the balance gained.
 */
function QuoteCheckout({
  session,
  account,
  network,
  appearance,
  onCredited,
  onNewTopUp,
}: {
  session: CreatedQuote;
  account: Account;
  network: Network | undefined;
  appearance: Appearance;
  onCredited: () => void;
  onNewTopUp: () => void;
}) {
  const [status, setStatus] = useState<CheckoutStatus>("loading");
  const testnet = network?.testnet ?? true;
  const bps = assetOf(network, session.asset)?.bonus_bps ?? 0;
  const symbol = session.asset.toUpperCase();
  return (
    <div className="flex flex-col gap-4">
      {PAYABLE.has(status) && (
        <div data-testid="locked-rate" className="flex items-center justify-between gap-4 rounded-lg border bg-muted/40 px-4 py-3">
          <span className="flex min-w-0 items-center gap-2 text-sm text-muted-foreground">
            <TokenIcon asset={session.asset} className="size-5" />
            <span className="truncate">Locked rate · {tokenName(symbol, testnet)}</span>
          </span>
          <span className="shrink-0 text-sm font-semibold tabular-nums">{rate(symbol, session.exchange_rate)}</span>
        </div>
      )}
      {PAYABLE.has(status) && bps > 0 && (
        <p className="flex items-center gap-2 text-xs text-muted-foreground">
          <Gift className="size-3.5 text-success" aria-hidden="true" />
          Paying in {symbol} earns a +{percent(bps)} bonus, this demo merchant's promotion.
        </p>
      )}
      {status === "credited" ? (
        <Credited session={session} account={account} bps={bps} />
      ) : (
        <Suspense fallback={<CheckoutSkeleton />}>
          <Checkout
            clientSecret={session.client_secret}
            expectedAddress={session.expected_address}
            apiBase={account.api_base}
            appearance={appearance}
            onChange={(state) => setStatus(state.status)}
            onSuccess={onCredited}
          />
        </Suspense>
      )}
      <Button type="button" variant="outline" className="w-full" onClick={onNewTopUp}>
        Start a new top-up
      </Button>
    </div>
  );
}

/**
 * The credited payment: the credit, the demo merchant's bonus on it, and the new total, with the
 * paying transaction. The amounts come from the product's own ledger as it applies the webhook.
 */
function Credited({ session, account, bps }: { session: CreatedQuote; account: Account; bps: number }) {
  const row = account.payments.find((each) => each.quote === session.quote && each.id.startsWith("dep_"));
  const credit = session.amount ?? row?.amount ?? null;
  const bonus = row?.bonus ?? 0;
  const symbol = session.asset.toUpperCase();
  return (
    <Alert data-testid="payment-credited" role="status">
      <CircleCheck className="text-success!" aria-hidden="true" />
      <AlertTitle>Payment credited</AlertTitle>
      <AlertDescription className="text-foreground">
        <dl className="mt-1 grid w-full gap-1.5 tabular-nums">
          <div className="flex justify-between gap-3">
            <dt>Top-up</dt>
            <dd className="text-success">{credit === null ? "—" : dollars(credit)}</dd>
          </div>
          {bonus > 0 && (
            <div className="flex justify-between gap-3" data-testid="bonus-credited">
              <dt>
                {symbol} bonus{bps > 0 ? ` +${percent(bps)}` : ""}
                <span className="text-muted-foreground"> · this demo merchant's promotion</span>
              </dt>
              <dd className="text-success">{signedDollars(bonus)}</dd>
            </div>
          )}
          {bonus > 0 && credit !== null && (
            <div className="flex justify-between gap-3 border-t pt-1.5 font-medium">
              <dt>Total</dt>
              <dd className="text-success">{dollars(credit + bonus)}</dd>
            </div>
          )}
          {row?.tx_hash != null && (
            <div className="flex items-center justify-between gap-3">
              <dt className="text-muted-foreground">Transaction</dt>
              <dd>
                <ExplorerLink chainId={session.chain_id} kind="tx" value={row.tx_hash} copy />
              </dd>
            </div>
          )}
        </dl>
      </AlertDescription>
    </Alert>
  );
}

/**
 * Where to get the selected token on the selected network: a mintable test token's public mint,
 * from the visitor's wallet; another test token's issuer faucet; and the network's gas faucets.
 */
function TestTokens({ network, asset }: { network: Network; asset: Asset }) {
  const mint = useMutation({
    mutationFn: async () => {
      const { mintTestTokens } = await wallet();
      return mintTestTokens(network.chain_id, asset.contract, "1000", asset.decimals);
    },
  });
  const chain = network.name.replace(/ testnet$/, "");
  const row = "flex flex-col gap-3 px-5 py-4 sm:flex-row sm:items-center sm:px-6";
  return (
    <div role="note" aria-label="Test tokens" className="rounded-xl border bg-card text-sm text-card-foreground shadow-sm">
      <div className="flex items-center justify-between gap-2 border-b px-5 py-3 sm:px-6">
        <span className="font-medium">Need test tokens?</span>
        <InfoTip label="About test tokens" className="translate-y-0">
          {asset.mintable
            ? `Test ${asset.symbol} is free: its contract lets anyone mint it, so your own wallet mints it.`
            : `Test ${asset.symbol} is free from its issuer's faucet.`}{" "}
          Gas is {chain} ETH, also free, from a public faucet.
        </InfoTip>
      </div>
      <ul className="divide-y">
        <li className={row}>
          <span className="flex min-w-0 flex-1 items-center gap-3">
            <TokenIcon asset={asset.asset} className="size-6" />
            <span className="flex min-w-0 flex-col">
              <span className="font-medium">Test {asset.symbol}</span>
              <span className="text-xs text-muted-foreground" data-testid={asset.mintable ? undefined : "faucet-hint"}>
                {asset.mintable
                  ? `Minted by your wallet on ${chain}`
                  : asset.faucet !== null
                    ? `On the faucet, pick ${chain} as the network.`
                    : "Not available from a faucet"}
              </span>
            </span>
          </span>
          {asset.mintable ? (
            <Button type="button" size="sm" variant="outline" onClick={() => mint.mutate()} disabled={mint.isPending}>
              {mint.isPending ? "Confirm in your wallet…" : `Mint 1,000 test ${asset.symbol}`}
            </Button>
          ) : asset.faucet !== null ? (
            <Button asChild size="sm" variant="outline">
              <a href={asset.faucet} target="_blank" rel="noreferrer">
                Get test {asset.symbol} from Circle
                <ExternalLink aria-hidden="true" />
              </a>
            </Button>
          ) : null}
        </li>
        {network.faucet !== null && (
          <li className={row}>
            <span className="flex min-w-0 flex-1 items-center gap-3">
              <ChainIcon chainId={network.chain_id} className="size-6" />
              <span className="flex min-w-0 flex-col">
                <span className="font-medium">{chain} ETH</span>
                <span className="text-xs text-muted-foreground">For gas, from a public faucet</span>
              </span>
            </span>
            <Button asChild size="sm" variant="outline">
              <a href={network.faucet} target="_blank" rel="noreferrer">
                {chain} ETH faucets
                <ExternalLink aria-hidden="true" />
              </a>
            </Button>
          </li>
        )}
      </ul>
      <p aria-live="polite" className="border-t px-5 py-3 text-xs text-muted-foreground empty:hidden sm:px-6">
        {mint.isSuccess && (
          <>
            Minted: <ExplorerLink chainId={network.chain_id} kind="tx" value={mint.data} copy />
          </>
        )}
        {mint.isError && <span className="text-destructive">{errorMessage(mint.error, "Minting failed.")}</span>}
      </p>
    </div>
  );
}
