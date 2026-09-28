import { DepositAddress, type Appearance } from "@phala/pay/react";
import { CircleAlert, ShieldCheck } from "lucide-react";
import { useCallback, useId, useState, type FormEvent } from "react";
import { parseUnits } from "viem";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  createDepositAddress,
  getDepositAddress,
  type Account,
  type DepositAddressResponse,
  type Selection,
} from "./api.js";
import { Detail, Details, ExplorerLink, describe, usePolling } from "./common.js";
import { tokens } from "./format.js";
import { errorMessage, transferTokens } from "./testTokens.js";

/**
 * The customer's single deposit address: one address for every supported token on every network,
 * credited at spot on arrival. The product creates it with its API key and the SDK recomputes it
 * from the pins before it is shown; the browser follows its payments with the address's
 * `client_secret` through `<DepositAddress>`.
 */
export function DepositAddressPanel({
  account,
  appearance,
  onSelect,
}: {
  account: Account;
  appearance: Appearance;
  onSelect: (selection: Selection) => void;
}) {
  const [created, setCreated] = useState<DepositAddressResponse | null>(null);
  const [current, setCurrent] = useState<DepositAddressResponse | null>(null);
  const [state, setState] = useState<{ pending: boolean; error: string | null }>({ pending: false, error: null });
  const refresh = useCallback(() => {
    getDepositAddress().then(setCurrent, () => undefined);
  }, []);
  usePolling(refresh, 3000, created !== null);

  const show = () => {
    setState({ pending: true, error: null });
    createDepositAddress().then(
      (response) => {
        setCreated(response);
        setCurrent(response);
        setState({ pending: false, error: null });
      },
      (error: unknown) => setState({ pending: false, error: `Could not get the address: ${describe(error)}.` }),
    );
  };

  if (created === null || created.client_secret === undefined) {
    return (
      <div className="flex flex-col gap-4">
        <p>
          Your workspace's own address for top-ups of any amount, at any time: one address for every
          supported token on every supported network, reusable, and credited at the market rate when
          a payment arrives. Use it when you pay from an exchange or cannot hit an exact amount.
        </p>
        <Button type="button" size="lg" className="w-full" onClick={show} disabled={state.pending}>
          {state.pending ? "Getting your address…" : "Show my deposit address"}
        </Button>
        {state.error !== null && (
          <Alert variant="destructive">
            <CircleAlert aria-hidden="true" />
            <AlertDescription>{state.error}</AlertDescription>
          </Alert>
        )}
      </div>
    );
  }
  const view = (current ?? created).deposit_address;
  return (
    <div className="flex flex-col gap-4">
      <Alert role="status" className="border-success/40 bg-success/10" data-testid="deposit-address-verified">
        <ShieldCheck aria-hidden="true" />
        <AlertTitle className="text-success">Verified</AlertTitle>
        <AlertDescription className="text-xs">
          The product's SDK recomputed {view.address === null ? "every network's address" : "this address"} from its
          pinned account, factory, implementation, and treasury before showing it.
        </AlertDescription>
      </Alert>
      <Details>
        {view.address !== null ? (
          <Detail label="Address (every network)" className="font-mono" data-testid="deposit-address">
            {view.address}
          </Detail>
        ) : (
          view.networks.map((network) => (
            <Detail key={network.chain_id} label={`Chain ${network.chain_id}`} className="font-mono">
              {network.address}
            </Detail>
          ))
        )}
        <Detail label="Networks">
          {view.networks
            .map((network) => `${network.chain_id === account.network.chain_id ? account.network.name : `Chain ${network.chain_id}`}: ${network.assets.map((asset) => asset.asset.toUpperCase()).join(", ")}`)
            .join(" · ")}
        </Detail>
        <Detail label="Metadata" className="font-mono">
          {JSON.stringify(view.metadata)}
        </Detail>
      </Details>
      <div>
        <DepositAddress
          depositAddress={created.deposit_address}
          clientSecret={created.client_secret}
          apiBase={account.api_base}
          appearance={appearance}
        />
      </div>
      <PayFromWallet account={account} to={view.address ?? view.networks[0]?.address ?? ""} />
      <section className="flex flex-col gap-2 border-t pt-4" aria-labelledby="address-payments-title">
        <h4 id="address-payments-title" className="font-medium">
          Payments the product sees
        </h4>
        {view.payments.length === 0 ? (
          <p className="text-xs text-muted-foreground">None yet. Send any amount of {account.token.symbol} to the address.</p>
        ) : (
          <ul className="flex flex-col gap-1.5 text-xs" aria-live="polite">
            {view.payments.map((payment) => (
              <li key={payment.deposit} data-testid="address-payment">
                {tokens(payment.amount_atomic, account.token.symbol)}{" "}
                {payment.status === "seen"
                  ? `received, ${payment.confirmations ?? 0} confirmation${payment.confirmations === 1 ? "" : "s"}`
                  : "recorded as a deposit"}{" "}
                · <ExplorerLink account={account} kind="tx" value={payment.tx_hash} /> ·{" "}
                <Button
                  type="button"
                  variant="link"
                  className="h-auto p-0 text-xs"
                  onClick={() => onSelect({ kind: "deposit", id: payment.deposit })}
                >
                  Timeline
                </Button>
              </li>
            ))}
          </ul>
        )}
      </section>
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
    transferTokens(account.network.chain_id, account.token.address, to, atomic).then(
      (hash) => setState({ pending: false, text: `Sent: ${hash}` }),
      (error: unknown) => setState({ pending: false, text: errorMessage(error, "The wallet did not send it.") }),
    );
  };
  return (
    <form className="flex flex-col gap-2" onSubmit={submit} aria-label="Pay to the deposit address from a browser wallet">
      <FieldLabel htmlFor={id}>Send from your browser wallet ({account.token.symbol})</FieldLabel>
      <div className="flex gap-2">
        <Input id={id} inputMode="decimal" value={amount} onChange={(event) => setAmount(event.target.value)} />
        <Button type="submit" variant="outline" disabled={state.pending || to === ""}>
          {state.pending ? "Confirm in your wallet…" : "Send"}
        </Button>
      </div>
      <p className="text-xs text-muted-foreground wrap-anywhere" aria-live="polite">
        {state.text}
      </p>
    </form>
  );
}
