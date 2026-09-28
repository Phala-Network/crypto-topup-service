import type { Appearance } from "@phala/pay/react";
import { CircleAlert } from "lucide-react";
import { Suspense, lazy, useId, useState, type FormEvent } from "react";
import { parseUnits } from "viem";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import { Skeleton } from "@/components/ui/skeleton";
import { createDepositAddress, type Account, type DepositAddressResponse } from "./api.js";
import { BRAND_BUTTON, InfoTip, describe, errorMessage, loadSdk, wallet } from "./common.js";

const DepositAddress = lazy(() => loadSdk().then((sdk) => ({ default: sdk.DepositAddress })));

/**
 * The customer's single deposit address: one address for every supported token on every network,
 * credited at spot on arrival. The product creates it with its API key and the SDK recomputes it
 * from the pins before it is shown (./Backend shows that check); the browser follows its payments
 * with the address's `client_secret` through `<DepositAddress>`.
 */
export function DepositAddressPanel({
  account,
  appearance,
  created,
  onCreated,
}: {
  account: Account;
  appearance: Appearance;
  created: DepositAddressResponse | null;
  onCreated: (created: DepositAddressResponse) => void;
}) {
  const [state, setState] = useState<{ pending: boolean; error: string | null }>({ pending: false, error: null });

  const show = () => {
    setState({ pending: true, error: null });
    loadSdk().catch(() => undefined);
    createDepositAddress().then(
      (response) => {
        onCreated(response);
        setState({ pending: false, error: null });
      },
      (error: unknown) => setState({ pending: false, error: `Could not get the address: ${describe(error)}.` }),
    );
  };

  if (created === null || created.client_secret === undefined) {
    return (
      <div className="flex flex-col gap-4">
        <Button type="button" size="lg" className={BRAND_BUTTON} onClick={show} disabled={state.pending}>
          {state.pending ? "Getting your address…" : "Show my deposit address"}
        </Button>
        <p className="flex items-center justify-center gap-1.5 text-center text-xs text-muted-foreground">
          Any amount, any time, at the market rate
          <InfoTip label="About the deposit address">
            Your workspace's own address for top-ups of any amount, at any time: one address for every supported token
            on every supported network, reusable, and credited at the market rate when a payment arrives. Use it when
            you pay from an exchange or cannot hit an exact amount.
          </InfoTip>
        </p>
        {state.error !== null && (
          <Alert variant="destructive">
            <CircleAlert aria-hidden="true" />
            <AlertDescription>{state.error}</AlertDescription>
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
          depositAddress={created.deposit_address}
          clientSecret={created.client_secret}
          apiBase={account.api_base}
          appearance={appearance}
        />
      </Suspense>
      <PayFromWallet account={account} to={view.address ?? view.networks[0]?.address ?? ""} />
    </div>
  );
}

function PayFromWallet({ account, to }: { account: Account; to: string }) {
  const [amount, setAmount] = useState("25");
  const [state, setState] = useState<{ pending: boolean; text: string | null }>({ pending: false, text: null });
  const id = useId();
  const submit = (event: FormEvent) => {
    event.preventDefault();
    let atomic: bigint;
    try {
      atomic = parseUnits(amount.trim(), 18);
    } catch {
      setState({ pending: false, text: `Enter an amount of ${account.token.symbol}.` });
      return;
    }
    setState({ pending: true, text: null });
    wallet()
      .then(({ transferTokens }) => transferTokens(account.network.chain_id, account.token.address, to, atomic))
      .then(
        (hash) => setState({ pending: false, text: `Sent: ${hash}` }),
        (error: unknown) => setState({ pending: false, text: errorMessage(error, "The wallet did not send it.") }),
      );
  };
  return (
    <form
      className="flex flex-col gap-2 border-t pt-5"
      onSubmit={submit}
      aria-label="Pay to the deposit address from a browser wallet"
    >
      <FieldLabel htmlFor={id}>Send from your browser wallet ({account.token.symbol})</FieldLabel>
      <div className="flex gap-2">
        <Input
          id={id}
          className="h-9 tabular-nums"
          inputMode="decimal"
          value={amount}
          onChange={(event) => setAmount(event.target.value)}
        />
        <Button type="submit" variant="outline" size="lg" disabled={state.pending || to === ""}>
          {state.pending ? "Confirm in your wallet…" : "Send"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground wrap-anywhere empty:hidden" aria-live="polite">
        {state.text}
      </p>
    </form>
  );
}
