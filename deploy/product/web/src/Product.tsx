import type { Appearance } from "@phala/pay/react";
import { AppWindow, CircleAlert, Lock } from "lucide-react";
import { Suspense, lazy, useId, useState, type FormEvent, type ReactNode } from "react";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel, FieldLegend, FieldSet } from "@/components/ui/field";
import { InputGroup, InputGroupAddon, InputGroupInput, InputGroupText } from "@/components/ui/input-group";
import { RadioGroup, RadioGroupItem } from "@/components/ui/radio-group";
import { Skeleton } from "@/components/ui/skeleton";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { createQuote, type Account, type CreatedQuote, type DepositAddressResponse } from "./api.js";
import { BRAND_BUTTON, InfoTip, describe, loadSdk } from "./common.js";
import { DepositAddressPanel } from "./DepositAddressPanel.js";
import { dollars } from "./format.js";

const Checkout = lazy(() => loadSdk().then((sdk) => ({ default: sdk.Checkout })));

export type Method = "quote" | "address";

export interface Session {
  quote: string;
  clientSecret: string;
  expectedAddress: string;
}

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
  method: Method;
  onMethodChange: (method: Method) => void;
  session: Session | null;
  onQuote: (created: CreatedQuote) => void;
  onNewTopUp: () => void;
  onCredited: () => void;
  address: DepositAddressResponse | null;
  onAddress: (created: DepositAddressResponse) => void;
  appearance: Appearance;
}) {
  return (
    <div className="flex min-w-0 flex-col gap-3 lg:sticky lg:top-20">
      <AreaLabel icon={<AppWindow />} title="Your product" text="What your customer sees" />
      <section
        aria-labelledby="product-title"
        className="product-app overflow-hidden rounded-2xl border bg-card text-card-foreground shadow-[0_1px_2px_rgb(0_0_0/0.04),0_12px_40px_-12px_rgb(0_0_0/0.12)] dark:shadow-[0_12px_40px_-12px_rgb(0_0_0/0.6)]"
      >
        <div className="grid h-10 grid-cols-[3rem_1fr_3rem] items-center border-b bg-muted/50 px-3.5" aria-hidden="true">
          <span className="flex gap-1.5">
            <span className="size-2.5 rounded-full bg-foreground/12" />
            <span className="size-2.5 rounded-full bg-foreground/12" />
            <span className="size-2.5 rounded-full bg-foreground/12" />
          </span>
          <span className="mx-auto flex items-center gap-1.5 rounded-md bg-background px-3 py-1 text-[0.6875rem] text-muted-foreground ring-1 ring-border">
            <Lock className="size-2.5" />
            Cloud Console · Billing
          </span>
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
              <TabsContent value="quote" className="pt-4">
                {session === null || account === null ? (
                  <AmountPicker account={account} onQuote={onQuote} />
                ) : (
                  <div className="flex flex-col gap-5">
                    <Suspense fallback={<CheckoutSkeleton />}>
                      <Checkout
                        clientSecret={session.clientSecret}
                        expectedAddress={session.expectedAddress}
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
              <TabsContent value="address" className="pt-4">
                {account === null ? (
                  <CheckoutSkeleton />
                ) : (
                  <DepositAddressPanel account={account} appearance={appearance} created={address} onCreated={onAddress} />
                )}
              </TabsContent>
            </Tabs>
          </section>
        </div>
      </section>
    </div>
  );
}

/** A small caption above each of the page's two areas. */
export function AreaLabel({ icon, title, text }: { icon: ReactNode; title: string; text: string }) {
  return (
    <p className="flex items-center gap-2 px-1 text-xs [&_svg]:size-3.5 [&_svg]:text-muted-foreground">
      {icon}
      <span className="font-medium">{title}</span>
      <span className="text-muted-foreground">· {text}</span>
    </p>
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
    // The checkout's code loads while the quote is created.
    loadSdk().catch(() => undefined);
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
        <FieldLegend variant="label" className="sr-only">
          Amount
        </FieldLegend>
        <RadioGroup
          value={String(preset)}
          onValueChange={(value) => setPreset(value === "custom" ? "custom" : Number(value))}
          className="grid-cols-2 gap-2 sm:grid-cols-4"
        >
          {options.map((option) => (
            <FieldLabel
              key={option.value}
              htmlFor={`${id}-${option.value}`}
              className="cursor-pointer transition-colors has-data-checked:border-foreground/70! has-data-checked:bg-transparent! has-data-checked:ring-1 has-data-checked:ring-foreground/70 dark:has-data-checked:bg-transparent!"
            >
              <Field orientation="horizontal" className="justify-center py-3!">
                <RadioGroupItem value={option.value} id={`${id}-${option.value}`} className="sr-only" />
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
      <Button type="submit" size="lg" className={BRAND_BUTTON} disabled={pending || account === null}>
        {pending ? "Creating quote…" : "Pay with crypto"}
      </Button>
      <p className="flex items-center justify-center gap-1.5 text-xs text-muted-foreground">
        Price locked for 15 minutes
        <InfoTip label="About paying for an exact amount">
          A quote locks the price for 15 minutes for an exact amount. Pay from a browser wallet, by QR code, or by
          sending the exact amount manually; another amount, or a late payment, is credited at the market rate
          instead.
        </InfoTip>
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
