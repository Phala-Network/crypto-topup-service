import { DepositAddress, type Appearance } from "@phala/pay/react";
import { useCallback, useId, useState, type FormEvent } from "react";
import { parseUnits } from "viem";
import {
  createDepositAddress,
  getDepositAddress,
  type Account,
  type DepositAddressResponse,
  type Selection,
} from "./api.js";
import { ExplorerLink, describe, usePolling } from "./common.js";
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
      <div className="stack">
        <p className="small">
          Your workspace's own address for top-ups of any amount, at any time: one address for every
          supported token on every supported network, reusable, and credited at the market rate when
          a payment arrives. Use it when you pay from an exchange or cannot hit an exact amount.
        </p>
        <button type="button" className="primary" onClick={show} disabled={state.pending}>
          {state.pending ? "Getting your address…" : "Show my deposit address"}
        </button>
        {state.error !== null && (
          <p className="alert" role="alert">
            {state.error}
          </p>
        )}
      </div>
    );
  }
  const view = (current ?? created).deposit_address;
  return (
    <div className="stack">
      <div className="verified" data-testid="deposit-address-verified">
        <span className="success">✓ Verified</span>{" "}
        <span className="small">
          The product's SDK recomputed {view.address === null ? "every network's address" : "this address"} from its
          pinned account, factory, implementation, and treasury before showing it.
        </span>
      </div>
      <dl className="details">
        {view.address !== null ? (
          <div>
            <dt>Address (every network)</dt>
            <dd className="mono" data-testid="deposit-address">
              {view.address}
            </dd>
          </div>
        ) : (
          view.networks.map((network) => (
            <div key={network.chain_id}>
              <dt>Chain {network.chain_id}</dt>
              <dd className="mono">{network.address}</dd>
            </div>
          ))
        )}
        <div>
          <dt>Networks</dt>
          <dd>
            {view.networks
              .map((network) => `${network.chain_id === account.network.chain_id ? account.network.name : `Chain ${network.chain_id}`}: ${network.assets.map((asset) => asset.asset.toUpperCase()).join(", ")}`)
              .join(" · ")}
          </dd>
        </div>
        <div>
          <dt>Metadata</dt>
          <dd className="mono">{JSON.stringify(view.metadata)}</dd>
        </div>
      </dl>
      <div className="checkout">
        <DepositAddress
          depositAddress={created.deposit_address}
          clientSecret={created.client_secret}
          apiBase={account.api_base}
          appearance={appearance}
        />
      </div>
      <PayFromWallet account={account} to={view.address ?? view.networks[0]?.address ?? ""} />
      <section aria-labelledby="address-payments-title">
        <h3 id="address-payments-title">Payments the product sees</h3>
        {view.payments.length === 0 ? (
          <p className="muted small">None yet. Send any amount of {account.token.symbol} to the address.</p>
        ) : (
          <ul className="plain" aria-live="polite">
            {view.payments.map((payment) => (
              <li key={payment.deposit} data-testid="address-payment">
                {tokens(payment.amount_atomic, account.token.symbol)}{" "}
                {payment.status === "seen"
                  ? `received, ${payment.confirmations ?? 0} confirmation${payment.confirmations === 1 ? "" : "s"}`
                  : "recorded as a deposit"}{" "}
                · <ExplorerLink account={account} kind="tx" value={payment.tx_hash} /> ·{" "}
                <button type="button" className="link" onClick={() => onSelect({ kind: "deposit", id: payment.deposit })}>
                  Timeline
                </button>
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
    <form className="inline-form" onSubmit={submit} aria-label="Pay to the deposit address from a browser wallet">
      <label htmlFor={id} className="small">
        Send from your browser wallet ({account.token.symbol})
      </label>
      <div className="inline">
        <input id={id} className="input" inputMode="decimal" value={amount} onChange={(event) => setAmount(event.target.value)} />
        <button type="submit" className="secondary" disabled={state.pending || to === ""}>
          {state.pending ? "Confirm in your wallet…" : "Send"}
        </button>
      </div>
      <p className="muted small" aria-live="polite">
        {state.text}
      </p>
    </form>
  );
}
