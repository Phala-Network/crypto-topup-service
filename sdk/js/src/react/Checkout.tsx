"use client";

import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import type { Hash } from "viem";
import type { CheckoutStatus } from "../checkout.js";
import { networkName, transactionUrl } from "../chains.js";
import { formatAmount, formatCountdown, formatTokenAmount, tokenAmount } from "../format.js";
import { quoteTransfer } from "../payment.js";
import type { ClientQuote } from "../quote.js";
import { WalletError, payWithWallet, watchWallets, type Wallet } from "../wallet.js";
import { STYLES, appearanceStyle, type Appearance } from "./appearance.js";
import { QrCode } from "./QrCode.js";
import { useCheckout } from "./useCheckout.js";

export interface CheckoutProps {
  /** The quote's `client_secret`, from your backend's `POST /v1/quotes`. */
  clientSecret: string;
  /** The service origin, for example `https://topup.example.com`. */
  apiBase: string;
  /** Called once when the payment is credited. Fulfil from the `deposit.credited` webhook, not here. */
  onSuccess?: (quote: ClientQuote) => void;
  /** Called once when the quote expires or is canceled without a payment. */
  onExpire?: (quote: ClientQuote) => void;
  appearance?: Appearance;
  /** Milliseconds between status reads; default 3000. */
  pollInterval?: number;
  className?: string;
  /** The wallet button's label; default "Pay with crypto". */
  buttonText?: string;
}

type Method = "wallet" | "qr" | "manual";

const METHODS: { id: Method; label: string }[] = [
  { id: "wallet", label: "Browser wallet" },
  { id: "qr", label: "QR code" },
  { id: "manual", label: "Send manually" },
];

/** A checkout for one quote: pay from a browser wallet, by QR code, or manually, with live status. */
export function Checkout({
  clientSecret,
  apiBase,
  onSuccess,
  onExpire,
  appearance,
  pollInterval,
  className,
  buttonText = "Pay with crypto",
}: CheckoutProps) {
  const { status, quote, error, refresh } = useCheckout({
    clientSecret,
    apiBase,
    ...(pollInterval === undefined ? {} : { pollInterval }),
  });
  const [txHash, setTxHash] = useState<Hash | null>(null);
  const now = useNow(status === "waiting");

  const callbacks = useRef({ onSuccess, onExpire });
  useEffect(() => {
    callbacks.current = { onSuccess, onExpire };
  });
  const notified = useRef<string | null>(null);
  useEffect(() => {
    if (quote === null || notified.current === quote.id) {
      return;
    }
    if (status === "credited") {
      notified.current = quote.id;
      callbacks.current.onSuccess?.(quote);
    } else if (status === "expired" || status === "canceled") {
      notified.current = quote.id;
      callbacks.current.onExpire?.(quote);
    }
  }, [status, quote]);

  return (
    <div
      className={className === undefined ? "pp-root" : `pp-root ${className}`}
      data-theme={appearance?.theme ?? "light"}
      style={appearanceStyle(appearance)}
    >
      <style>{STYLES}</style>
      {quote !== null && (
        <>
          <p className="pp-amount">
            {formatTokenAmount(quote)} {quote.asset.toUpperCase()}
          </p>
          <p className="pp-subtitle">
            {formatAmount(quote)} top-up · {networkName(quote.chain_id)}
          </p>
        </>
      )}
      <StatusLine status={status} quote={quote} now={now} reconnecting={error !== null} />
      {txHash !== null && quote !== null && <Transaction hash={txHash} chainId={quote.chain_id} />}
      {status === "waiting" && quote !== null && (
        <PaymentOptions
          quote={quote}
          buttonText={buttonText}
          now={now}
          onSent={(hash) => {
            setTxHash(hash);
            refresh();
          }}
        />
      )}
    </div>
  );
}

function StatusLine({
  status,
  quote,
  now,
  reconnecting,
}: {
  status: CheckoutStatus;
  quote: ClientQuote | null;
  now: number;
  reconnecting: boolean;
}) {
  const tone =
    status === "credited"
      ? "success"
      : ["rejected", "expired", "canceled", "error"].includes(status)
        ? "danger"
        : "neutral";
  return (
    <div className="pp-status" data-tone={tone}>
      <span role="status" aria-live="polite">
        {statusMessage(status, quote)}
        {reconnecting && status !== "error" ? " (reconnecting…)" : ""}
      </span>
      {status === "waiting" && quote !== null && (
        <span className="pp-countdown" aria-label="Time left to pay">
          {formatCountdown(quote.expires_at, now)}
        </span>
      )}
    </div>
  );
}

function statusMessage(status: CheckoutStatus, quote: ClientQuote | null): string {
  switch (status) {
    case "loading":
      return "Loading payment details…";
    case "waiting":
      return "Waiting for your payment";
    case "seen":
      return quote?.confirmations == null
        ? "Payment received, waiting for confirmations"
        : `Payment received, ${quote.confirmations} confirmation${quote.confirmations === 1 ? "" : "s"}`;
    case "confirming":
      return "Payment confirmed on chain, crediting…";
    case "credited":
      return quote === null ? "Payment credited" : `Payment credited: ${formatAmount(quote)}`;
    case "rejected":
      return "This payment cannot be credited. Contact support with your transaction.";
    case "expired":
      return "This quote has expired. Do not send funds to this address; start a new top-up.";
    case "canceled":
      return "This quote was canceled. Do not send funds to this address.";
    case "error":
      return "This payment link is not valid. Start a new top-up.";
  }
}

function Transaction({ hash, chainId }: { hash: Hash; chainId: number }) {
  const url = transactionUrl(chainId, hash);
  const short = `${hash.slice(0, 10)}…${hash.slice(-8)}`;
  return (
    <p className="pp-tx">
      Transaction sent:{" "}
      {url === undefined ? (
        <span className="pp-value" title={hash}>
          {short}
        </span>
      ) : (
        <a href={url} target="_blank" rel="noreferrer" title={hash}>
          {short}
        </a>
      )}
    </p>
  );
}

function PaymentOptions({
  quote,
  now,
  buttonText,
  onSent,
}: {
  quote: ClientQuote;
  now: number;
  buttonText: string;
  onSent: (hash: Hash) => void;
}) {
  const [method, setMethod] = useState<Method>("wallet");
  const id = useId();
  const tabs = useRef<(HTMLButtonElement | null)[]>([]);
  const amount = `${formatTokenAmount(quote)} ${quote.asset.toUpperCase()}`;

  const onKeyDown = (event: KeyboardEvent<HTMLDivElement>) => {
    const index = METHODS.findIndex((m) => m.id === method);
    const next =
      event.key === "ArrowRight"
        ? (index + 1) % METHODS.length
        : event.key === "ArrowLeft"
          ? (index - 1 + METHODS.length) % METHODS.length
          : event.key === "Home"
            ? 0
            : event.key === "End"
              ? METHODS.length - 1
              : null;
    const target = next === null ? undefined : METHODS[next];
    if (next !== null && target !== undefined) {
      event.preventDefault();
      setMethod(target.id);
      tabs.current[next]?.focus();
    }
  };

  return (
    <>
      <p className="pp-notice">
        Send <strong>exactly {amount}</strong> on {networkName(quote.chain_id)} in one transfer
        before the timer ends. A different amount, or a payment after expiry, is credited at the
        market price instead of this quote. Exchanges may deduct a withdrawal fee, so the amount
        that arrives must be exact.
      </p>
      <div className="pp-tabs" role="tablist" aria-label="Payment method" onKeyDown={onKeyDown}>
        {METHODS.map((m, index) => (
          <button
            key={m.id}
            ref={(element) => {
              tabs.current[index] = element;
            }}
            type="button"
            role="tab"
            className="pp-tab"
            id={`${id}-tab-${m.id}`}
            aria-selected={method === m.id}
            aria-controls={`${id}-panel-${m.id}`}
            tabIndex={method === m.id ? 0 : -1}
            onClick={() => setMethod(m.id)}
          >
            {m.label}
          </button>
        ))}
      </div>
      <div
        role="tabpanel"
        id={`${id}-panel-${method}`}
        aria-labelledby={`${id}-tab-${method}`}
        tabIndex={0}
      >
        {method === "wallet" && <WalletPanel quote={quote} buttonText={buttonText} onSent={onSent} />}
        {method === "qr" && (
          <div className="pp-qr-panel">
            <QrCode value={quote.payment_uri} label={`Payment request for ${amount}`} />
            <p className="pp-message">
              Scan with a wallet app that reads payment links, and check that it shows {amount} on{" "}
              {networkName(quote.chain_id)} before you confirm.
            </p>
          </div>
        )}
        {method === "manual" && <ManualPanel quote={quote} now={now} />}
      </div>
    </>
  );
}

type WalletStep =
  | { kind: "idle" }
  | { kind: "pending"; wallet: string }
  | { kind: "failed"; message: string };

function WalletPanel({
  quote,
  buttonText,
  onSent,
}: {
  quote: ClientQuote;
  buttonText: string;
  onSent: (hash: Hash) => void;
}) {
  const [wallets, setWallets] = useState<Wallet[]>([]);
  const [step, setStep] = useState<WalletStep>({ kind: "idle" });
  useEffect(() => watchWallets(setWallets), []);

  const pay = async (wallet: Wallet) => {
    setStep({ kind: "pending", wallet: wallet.info.name });
    try {
      onSent(await payWithWallet(wallet.provider, quote));
      setStep({ kind: "idle" });
    } catch (error) {
      setStep({
        kind: "failed",
        message: error instanceof WalletError ? error.message : "The payment could not be sent",
      });
    }
  };

  if (wallets.length === 0) {
    return (
      <p className="pp-message">
        No browser wallet found. Scan the QR code with a mobile wallet, or send the payment
        manually.
      </p>
    );
  }
  return (
    <div className="pp-wallets">
      {wallets.map((wallet) => (
        <button
          key={wallet.info.uuid}
          type="button"
          className="pp-button"
          disabled={step.kind === "pending"}
          onClick={() => void pay(wallet)}
          aria-label={`${buttonText} (${wallet.info.name})`}
        >
          {wallet.info.icon !== "" && <img src={wallet.info.icon} alt="" />}
          <span>{buttonText}</span>
          <span className="pp-wallet-name">{wallet.info.name}</span>
        </button>
      ))}
      <p
        className="pp-message"
        data-tone={step.kind === "failed" ? "danger" : undefined}
        aria-live="polite"
      >
        {step.kind === "pending" && `Confirm the payment in ${step.wallet}…`}
        {step.kind === "failed" && step.message}
      </p>
    </div>
  );
}

function ManualPanel({ quote, now }: { quote: ClientQuote; now: number }) {
  const token = quoteTransfer(quote).token;
  return (
    <dl className="pp-fields">
      <Field label="Network" value={`${networkName(quote.chain_id)} (chain ID ${quote.chain_id})`} />
      <Field label={`Token (${quote.asset.toUpperCase()}) contract`} value={token} copy />
      <Field label="Send to address" value={quote.address} copy />
      <Field label="Exact amount" value={formatTokenAmount(quote)} copy={tokenAmount(quote)} />
      <Field label="Time left" value={formatCountdown(quote.expires_at, now)} />
    </dl>
  );
}

/** `copy` adds a copy button, copying `value`, or the given string when it differs from it. */
function Field({
  label,
  value,
  copy = false,
}: {
  label: string;
  value: string;
  copy?: boolean | string;
}) {
  return (
    <div className="pp-field">
      <dt>{label}</dt>
      <dd>
        <span className={copy === false ? undefined : "pp-value"}>{value}</span>
        {copy !== false && (
          <CopyButton value={typeof copy === "string" ? copy : value} label={label} />
        )}
      </dd>
    </div>
  );
}

function CopyButton({ value, label }: { value: string; label: string }) {
  const [copied, setCopied] = useState<boolean | null>(null);
  useEffect(() => {
    if (copied === null) {
      return;
    }
    const timer = setTimeout(() => setCopied(null), 2000);
    return () => clearTimeout(timer);
  }, [copied]);
  const onClick = () => {
    navigator.clipboard.writeText(value).then(
      () => setCopied(true),
      () => setCopied(false),
    );
  };
  return (
    <button type="button" className="pp-copy" onClick={onClick} aria-label={`Copy ${label}`}>
      <span aria-live="polite">{copied === null ? "Copy" : copied ? "Copied" : "Copy failed"}</span>
    </button>
  );
}

/** The current time in milliseconds, updated every second while `active`. */
function useNow(active: boolean): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    if (!active) {
      return;
    }
    const timer = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(timer);
  }, [active]);
  return now;
}
