import type { Appearance } from "@phala/pay/react";
import { useMutation } from "@tanstack/react-query";
import { CircleAlert } from "lucide-react";
import { Suspense, lazy, useId, useState, type FormEvent, type ReactNode } from "react";
import { parseUnits } from "viem";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { cn } from "@/lib/utils";
import type { Account, Asset, DepositAddressResponse } from "./api.js";
import { BRAND_BUTTON, ExplorerLink, InfoTip, describe, errorMessage, loadSdk, wallet } from "./common.js";
import { dollars, price, statusLabel, tokenName, tokens } from "./format.js";
import { useCreateDepositAddress } from "./queries.js";

const DepositAddress = lazy(() => loadSdk().then((sdk) => ({ default: sdk.DepositAddress })));

/**
 * The customer's single deposit address: one address for every supported token on every network,
 * credited at spot on arrival. The product creates it with its API key and the SDK recomputes it
 * from the pins before it is shown (./Backend shows that check); the browser follows its payments
 * with the address's `client_secret` through `<DepositAddress>`.
 */
export function DepositAddressPanel({
  account,
  picker,
  asset,
  appearance,
  created,
  onCreated,
}: {
  account: Account;
  /** The token choice, shown until the address is. */
  picker: ReactNode;
  asset: Asset | undefined;
  appearance: Appearance;
  created: DepositAddressResponse | null;
  onCreated: (created: DepositAddressResponse) => void;
}) {
  const show = useCreateDepositAddress();

  if (created === null || created.client_secret === undefined) {
    return (
      <div className="flex flex-col gap-5">
        {picker}
        <div className="flex flex-col gap-3">
          <Button
            type="button"
            size="lg"
            className={BRAND_BUTTON}
            onClick={() => {
              loadSdk().catch(() => undefined);
              show.mutate(undefined, { onSuccess: onCreated });
            }}
            disabled={show.isPending}
          >
            {show.isPending ? "Getting your address…" : "Show my deposit address"}
          </Button>
          <p className="flex items-center justify-center gap-1.5 text-center text-xs text-muted-foreground">
            Any amount, any time, at the market rate
            <InfoTip label="About the deposit address">
              Your workspace's own address for top-ups of any amount, at any time: one address for every supported
              token on every supported network, reusable, and credited at the market rate when a payment arrives. Use
              it when you pay from an exchange or cannot hit an exact amount.
            </InfoTip>
          </p>
        </div>
        {show.error !== null && (
          <Alert variant="destructive">
            <CircleAlert aria-hidden="true" />
            <AlertDescription>Could not get the address: {describe(show.error)}.</AlertDescription>
          </Alert>
        )}
      </div>
    );
  }
  const view = created.deposit_address;
  return (
    <div className="flex flex-col gap-6">
      <Suspense
        fallback={
          <div className="flex flex-col gap-3" aria-hidden="true">
            <Skeleton className="mx-auto size-40" />
            <Skeleton className="h-10 w-full" />
          </div>
        }
      >
        <DepositAddress
          depositAddress={view}
          clientSecret={created.client_secret}
          apiBase={account.api_base}
          appearance={appearance}
          {...(asset === undefined ? {} : { asset: asset.asset, chainId: asset.chain_id })}
        />
      </Suspense>
      <TopUps account={account} />
      <PayFromWallet account={account} asset={asset} to={view.address ?? view.networks[0]?.address ?? ""} />
    </div>
  );
}

/** The address's recorded payments, each at the rate it was credited at (`deposit.exchange_rate`). */
function TopUps({ account }: { account: Account }) {
  const deposits = account.payments.filter((row) => row.kind === "address" && row.id.startsWith("dep_"));
  if (deposits.length === 0) {
    return null;
  }
  const testnet = account.network.testnet;
  return (
    <section aria-labelledby="top-ups-title" className="flex flex-col gap-2">
      <h4 id="top-ups-title" className="text-[0.8125rem] font-medium">
        Your top-ups
      </h4>
      <ul className="flex flex-col divide-y rounded-lg border text-xs" data-testid="top-ups">
        {deposits.map((row) => {
          const symbol = (row.asset ?? account.token.symbol).toUpperCase();
          const reversed = row.status === "reversed" || row.status === "rejected";
          const valued = row.exchange_rate === null ? null : `${price(row.exchange_rate)} / ${symbol}`;
          return (
            <li key={row.id} data-testid="top-up" className="flex items-center justify-between gap-3 px-3 py-2.5">
              <span className="flex min-w-0 flex-col gap-0.5">
                <span className="font-medium tabular-nums">{tokens(row.amount_atomic, tokenName(symbol, testnet))}</span>
                <span className="text-muted-foreground tabular-nums">
                  {row.status === "rejected"
                    ? statusLabel(row.status)
                    : valued === null
                      ? "Valuing…"
                      : row.status === "reversed"
                        ? `Credited at ${valued}, then reversed`
                        : `Credited at ${valued}`}
                </span>
              </span>
              <span
                className={cn(
                  "shrink-0 text-sm font-medium tabular-nums",
                  reversed ? "text-muted-foreground line-through" : "text-success",
                )}
              >
                {row.amount === null ? "—" : `+${dollars(row.amount)}`}
              </span>
            </li>
          );
        })}
      </ul>
    </section>
  );
}

function PayFromWallet({ account, asset, to }: { account: Account; asset: Asset | undefined; to: string }) {
  const [amount, setAmount] = useState("25");
  const [invalid, setInvalid] = useState<string | null>(null);
  const id = useId();
  const symbol = asset?.symbol ?? account.token.symbol;
  const send = useMutation({
    mutationFn: async (atomic: bigint) => {
      const { transferTokens } = await wallet();
      return transferTokens(account.network.chain_id, asset?.contract ?? account.token.address, to, atomic);
    },
  });
  const submit = (event: FormEvent) => {
    event.preventDefault();
    let atomic: bigint;
    try {
      atomic = parseUnits(amount.trim(), asset?.decimals ?? 18);
    } catch {
      setInvalid(`Enter an amount of ${symbol}.`);
      return;
    }
    setInvalid(null);
    send.mutate(atomic);
  };

  return (
    <form
      className="flex flex-col gap-2 border-t pt-5"
      onSubmit={submit}
      aria-label="Pay to the deposit address from a browser wallet"
    >
      <FieldLabel htmlFor={id}>Send from your browser wallet ({tokenName(symbol, account.network.testnet)})</FieldLabel>
      <div className="flex gap-2">
        <Input
          id={id}
          className="h-10 tabular-nums"
          inputMode="decimal"
          value={amount}
          onChange={(event) => setAmount(event.target.value)}
        />
        <Button type="submit" variant="outline" size="lg" className="h-10" disabled={send.isPending || to === ""}>
          {send.isPending ? "Confirm in your wallet…" : "Send"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground wrap-anywhere empty:hidden" aria-live="polite">
        {invalid ?? (
          <>
            {send.isSuccess && (
              <>
                Sent: <ExplorerLink account={account} kind="tx" value={send.data} />
              </>
            )}
            {send.isError && errorMessage(send.error, "The wallet did not send it.")}
          </>
        )}
      </p>
    </form>
  );
}
