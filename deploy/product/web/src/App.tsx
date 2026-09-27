import { Checkout, type Appearance } from "@phala/pay/react";
import { useCallback, useEffect, useId, useState, type FormEvent } from "react";
import {
  ApiError,
  createQuote,
  getAccount,
  getTimeline,
  getTrust,
  type Account,
  type ApiExchange,
  type Detail,
  type Step,
  type StepKey,
  type Timeline,
  type Trust,
  type WebhookEvent,
} from "./api.js";
import { dollars, short, statusLabel, time, tokens } from "./format.js";
import { firstWallet, mintTestTokens } from "./testTokens.js";

type Theme = "light" | "dark";

interface Session {
  quote: string;
  clientSecret: string;
}

const STEP_COPY: Record<StepKey, { title: string; current: string; failed?: string }> = {
  quote_created: {
    title: "Quote created",
    current: "Creating the quote…",
  },
  transfer_seen: {
    title: "Transfer seen on chain",
    current: "Waiting for your transfer to the forwarder address…",
    failed: "The quote expired without a payment.",
  },
  finalized: {
    title: "Final on both RPC providers",
    current:
      "Waiting for Ethereum finality, usually 13–16 minutes after the transfer (64–96 blocks). " +
      "Credits count only final blocks, so a reorg can never undo one; both providers must then " +
      "report the same block and log.",
  },
  credited: {
    title: "Credited by the service",
    current: "Valuing the deposit and screening the sender…",
    failed: "The deposit was rejected and will not be credited.",
  },
  webhook_received: {
    title: "Webhook received by this product",
    current: "Waiting for the signed deposit.credited webhook…",
  },
  swept: {
    title: "Swept to the treasury Safe",
    current: "Swept with the next scheduled flush (every six hours, when gas allows).",
  },
};

export function App() {
  const [theme, setTheme] = useTheme();
  const [account, setAccount] = useState<Account | null>(null);
  const [accountError, setAccountError] = useState<string | null>(null);
  const [session, setSession] = useState<Session | null>(null);
  const [selected, setSelected] = useState<string | null>(null);
  const [timeline, setTimeline] = useState<Timeline | null>(null);
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

  const finished =
    timeline !== null &&
    timeline.quote.id === selected &&
    timeline.steps.some((s) => s.state === "failed" || (s.key === "swept" && s.state === "complete"));
  const refreshTimeline = useCallback(() => {
    if (selected === null) {
      return;
    }
    getTimeline(selected).then(setTimeline, () => undefined);
  }, [selected]);
  usePolling(refreshTimeline, 2000, selected !== null && !finished);

  const appearance: Appearance = {
    theme,
    variables: {
      colorPrimary: "var(--accent)",
      accessibleColorOnColorPrimary: "var(--accent-contrast)",
      colorBackground: "var(--surface)",
      colorText: "var(--text)",
      colorTextSecondary: "var(--text-muted)",
      colorBorder: "var(--border)",
      borderRadius: "10px",
      fontFamily: "var(--font)",
    },
  };

  return (
    <div className="app">
      {account?.network.testnet === true && <TestnetBanner account={account} />}
      <header className="topbar">
        <div className="brand">
          <span className="logo" aria-hidden="true" />
          <span>Cloud Console</span>
          <span className="crumb">/ Billing</span>
          <span className="pill">Phala Pay demo</span>
        </div>
        <button
          type="button"
          className="ghost"
          onClick={() => setTheme(theme === "dark" ? "light" : "dark")}
          aria-label={`Switch to ${theme === "dark" ? "light" : "dark"} theme`}
        >
          {theme === "dark" ? "Light" : "Dark"} theme
        </button>
      </header>

      <main className="page">
        <div className="page-head">
          <h1>Add credits</h1>
          <p className="muted">
            A cloud console's billing page paid with Phala Pay: top up this account with{" "}
            {account?.token.symbol ?? "PHA"} on {account?.network.name ?? "Sepolia"}. The balance
            moves only when this console's webhook handler receives a verified{" "}
            <code>deposit.credited</code>, exactly as a real integration applies credits.
          </p>
        </div>
        {accountError !== null && (
          <p className="alert" role="alert">
            Could not load the account: {accountError}
          </p>
        )}

        <div className="grid">
          <div className="column">
            <BalanceCard account={account} />
            <section className="card" aria-labelledby="pay-title">
              <h2 id="pay-title">Pay with crypto</h2>
              {session === null || account === null ? (
                <AmountPicker
                  account={account}
                  onQuote={(created) => {
                    setSession({ quote: created.quote, clientSecret: created.client_secret });
                    setSelected(created.quote);
                    setTimeline(null);
                  }}
                />
              ) : (
                <div className="checkout">
                  <Checkout
                    clientSecret={session.clientSecret}
                    apiBase={account.api_base}
                    appearance={appearance}
                    onSuccess={refreshAccount}
                  />
                  <button type="button" className="ghost" onClick={() => setSession(null)}>
                    Start a new top-up
                  </button>
                </div>
              )}
            </section>
          </div>
          <BehindTheScenes
            timeline={timeline !== null && timeline.quote.id === selected ? timeline : null}
            selected={selected}
            account={account}
          />
        </div>

        <Transactions
          account={account}
          selected={selected}
          onSelect={(quote) => {
            setSelected(quote);
            setTimeline(null);
          }}
        />
        <TrustStrip trust={trust} account={account} />
      </main>
    </div>
  );
}

function TestnetBanner({ account }: { account: Account }) {
  const [state, setState] = useState<{ kind: "idle" | "pending" | "done" | "failed"; text?: string }>(
    { kind: "idle" },
  );
  const mint = async () => {
    setState({ kind: "pending" });
    try {
      const wallet = await firstWallet();
      if (wallet === undefined) {
        setState({ kind: "failed", text: "No browser wallet found." });
        return;
      }
      const hash = await mintTestTokens(wallet, account.network.chain_id, account.token.address, "1000");
      setState({ kind: "done", text: hash });
    } catch (error) {
      const message = error instanceof Error ? error.message.split("\n")[0] : undefined;
      setState({ kind: "failed", text: message ?? "Minting failed." });
    }
  };
  return (
    <div className="banner" role="note">
      <strong>Testnet demo.</strong> {account.network.name} and test {account.token.symbol} only; no
      real money moves. Test {account.token.symbol} is free: mint it from your wallet (gas is{" "}
      {account.network.name} ETH from a public faucet).{" "}
      <button type="button" className="link" onClick={() => void mint()} disabled={state.kind === "pending"}>
        {state.kind === "pending" ? "Confirm in your wallet…" : `Get 1,000 test ${account.token.symbol}`}
      </button>
      <span aria-live="polite">
        {state.kind === "done" && state.text !== undefined && (
          <>
            {" "}
            Minted: <ExplorerLink account={account} kind="tx" value={state.text} />
          </>
        )}
        {state.kind === "failed" && ` ${state.text ?? ""}`}
      </span>
    </div>
  );
}

function BalanceCard({ account }: { account: Account | null }) {
  return (
    <section className="card balance" aria-labelledby="balance-title">
      <h2 id="balance-title">Account balance</h2>
      <p className="balance-value" aria-live="polite" data-testid="balance">
        {account === null ? "—" : dollars(account.balance)}
      </p>
      <p className="muted small">
        Demo account <code>{account?.account_id ?? "…"}</code>, kept in a cookie in this browser.
      </p>
    </section>
  );
}

function AmountPicker({
  account,
  onQuote,
}: {
  account: Account | null;
  onQuote: (created: Awaited<ReturnType<typeof createQuote>>) => void;
}) {
  const [preset, setPreset] = useState<number | "custom">(2000);
  const [custom, setCustom] = useState("");
  const [pending, setPending] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const customId = useId();
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

  return (
    <form onSubmit={submit} className="picker">
      <fieldset>
        <legend>Amount</legend>
        <div className="presets">
          {(account?.presets ?? [500, 2000, 5000]).map((cents) => (
            <label key={cents} className="preset">
              <input
                type="radio"
                name="amount"
                checked={preset === cents}
                onChange={() => setPreset(cents)}
              />
              <span>{dollars(cents)}</span>
            </label>
          ))}
          <label className="preset">
            <input
              type="radio"
              name="amount"
              checked={preset === "custom"}
              onChange={() => setPreset("custom")}
            />
            <span>Custom</span>
          </label>
        </div>
      </fieldset>
      {preset === "custom" && (
        <div className="field">
          <label htmlFor={customId}>Custom amount (USD)</label>
          <div className="money">
            <span aria-hidden="true">$</span>
            <input
              id={customId}
              inputMode="decimal"
              placeholder="25.00"
              value={custom}
              onChange={(event) => setCustom(event.target.value)}
            />
          </div>
        </div>
      )}
      <button type="submit" className="primary" disabled={pending || account === null}>
        {pending ? "Creating quote…" : "Pay with crypto"}
      </button>
      <p className="muted small">
        The price is locked for 15 minutes. Pay from a browser wallet, by QR code, or by sending
        the exact amount manually.
      </p>
      {error !== null && (
        <p className="alert" role="alert">
          {error}
        </p>
      )}
    </form>
  );
}

function BehindTheScenes({
  timeline,
  selected,
  account,
}: {
  timeline: Timeline | null;
  selected: string | null;
  account: Account | null;
}) {
  return (
    <aside className="card scenes" aria-labelledby="scenes-title">
      <div className="scenes-head">
        <h2 id="scenes-title">Behind the scenes</h2>
        {selected !== null && <span className="live" aria-hidden="true">Live</span>}
      </div>
      {selected === null ? (
        <p className="muted">
          Create a quote to follow the payment through the service, the chain, and this product's
          webhook handler, with real data only.
        </p>
      ) : timeline === null ? (
        <p className="muted">Loading {short(selected)}…</p>
      ) : (
        <>
          <ol className="timeline" aria-label="Payment timeline">
            {timeline.steps.map((step) => (
              <TimelineStep key={step.key} step={step} account={account} />
            ))}
          </ol>
          <EventsLog events={timeline.events} />
          <DeveloperView exchanges={timeline.api} />
        </>
      )}
    </aside>
  );
}

function TimelineStep({ step, account }: { step: Step; account: Account | null }) {
  const copy = STEP_COPY[step.key];
  return (
    <li
      className="step"
      data-step={step.key}
      data-state={step.state}
      aria-current={step.state === "current" ? "step" : undefined}
    >
      <span className="dot" aria-hidden="true" />
      <div className="step-body">
        <div className="step-title">
          <span>{copy.title}</span>
          <span className="step-state">{stateLabel(step.state)}</span>
        </div>
        {step.at !== null && <div className="muted small">{time(step.at)}</div>}
        {step.state === "current" && <div className="muted small">{copy.current}</div>}
        {step.state === "failed" && copy.failed !== undefined && (
          <div className="small danger">{copy.failed}</div>
        )}
        {step.details.length > 0 && (
          <dl className="details">
            {step.details.map((detail) => (
              <div key={detail.label}>
                <dt>{detail.label}</dt>
                <dd>
                  <DetailValue detail={detail} account={account} />
                </dd>
              </div>
            ))}
          </dl>
        )}
      </div>
    </li>
  );
}

function DetailValue({ detail, account }: { detail: Detail; account: Account | null }) {
  const { value, kind } = detail;
  if (value === null) {
    return <>—</>;
  }
  if ((kind === "address" || kind === "tx") && typeof value === "string" && account !== null) {
    return <ExplorerLink account={account} kind={kind} value={value} />;
  }
  if (kind === "time" && typeof value === "number") {
    return <>{time(value)}</>;
  }
  if (kind === "usd" && typeof value === "number") {
    return <>{dollars(value)}</>;
  }
  if (kind === "usd_delta" && typeof value === "number") {
    return <span className="success">+{dollars(value)}</span>;
  }
  if (kind === "atomic" && typeof value === "string") {
    return <>{tokens(value, account?.token.symbol ?? "")}</>;
  }
  return <span className={detail.mono === true ? "mono" : undefined}>{String(value)}</span>;
}

function EventsLog({ events }: { events: WebhookEvent[] }) {
  return (
    <section className="subsection" aria-labelledby="events-title">
      <h3 id="events-title">Webhook events received</h3>
      {events.length === 0 ? (
        <p className="muted small">None yet.</p>
      ) : (
        <div className="table-scroll">
          <table className="table compact">
            <thead>
              <tr>
                <th scope="col">Type</th>
                <th scope="col">Event id</th>
                <th scope="col">Received</th>
                <th scope="col">Signature</th>
              </tr>
            </thead>
            <tbody>
              {events.map((event) => (
                <tr key={event.id} data-testid="webhook-event">
                  <td>
                    <code>{event.type}</code>
                  </td>
                  <td className="mono" title={event.id}>
                    {short(event.id)}
                  </td>
                  <td>{time(event.received_at)}</td>
                  <td className="success">{event.verified ? "Verified" : "—"}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function DeveloperView({ exchanges }: { exchanges: ApiExchange[] }) {
  return (
    <details className="subsection dev">
      <summary>Developer view: the product's API requests ({exchanges.length})</summary>
      <p className="muted small">
        Signed with the product key on the server (RFC 9421); the browser never holds it.
      </p>
      {exchanges.map((exchange, index) => (
        <details key={`${exchange.method}-${exchange.url}-${index}`} className="exchange">
          <summary>
            <code>
              {exchange.method} {new URL(exchange.url).pathname}
              {new URL(exchange.url).search}
            </code>{" "}
            <span className={exchange.status < 400 ? "success" : "danger"}>{exchange.status}</span>
          </summary>
          <pre>{JSON.stringify({ request: exchange.request, response: exchange.response }, null, 2)}</pre>
        </details>
      ))}
    </details>
  );
}

function Transactions({
  account,
  selected,
  onSelect,
}: {
  account: Account | null;
  selected: string | null;
  onSelect: (quote: string) => void;
}) {
  const symbol = account?.token.symbol ?? "PHA";
  return (
    <section className="card" aria-labelledby="history-title">
      <h2 id="history-title">Transaction history</h2>
      {account === null || account.transactions.length === 0 ? (
        <p className="muted">No top-ups yet.</p>
      ) : (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Date</th>
                <th scope="col">Quote</th>
                <th scope="col">Amount</th>
                <th scope="col">{symbol} paid</th>
                <th scope="col">Transaction</th>
                <th scope="col">Status</th>
                <th scope="col">Credited</th>
                <th scope="col">Refunded</th>
                <th scope="col">
                  <span className="visually-hidden">Timeline</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {account.transactions.map((row) => (
                <tr key={row.quote} data-testid="transaction" aria-selected={row.quote === selected}>
                  <td>{time(row.created)}</td>
                  <td className="mono" title={row.quote}>
                    {short(row.quote)}
                  </td>
                  <td>{dollars(row.amount)}</td>
                  <td>{tokens(row.paid_atomic ?? row.amount_atomic, symbol)}</td>
                  <td>
                    {row.tx_hash === null ? "—" : <ExplorerLink account={account} kind="tx" value={row.tx_hash} />}
                  </td>
                  <td>
                    <span className={`badge ${row.status}`}>{statusLabel(row.status)}</span>
                  </td>
                  <td>{row.credited === null ? "—" : dollars(row.credited)}</td>
                  <td>{row.refunded_atomic === "0" ? "—" : tokens(row.refunded_atomic, symbol)}</td>
                  <td>
                    <button type="button" className="link" onClick={() => onSelect(row.quote)}>
                      Timeline
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
    </section>
  );
}

function TrustStrip({ trust, account }: { trust: Trust | null; account: Account | null }) {
  const attestation = trust?.attestation;
  const evidence = trust?.tls_evidence;
  return (
    <section className="trust" aria-labelledby="trust-title">
      <h2 id="trust-title">Why you can trust Phala Pay</h2>
      <div className="trust-grid">
        <div>
          <h3>Attestation</h3>
          {attestation === undefined ? (
            <p className="muted small">Loading…</p>
          ) : attestation.binding_verified ? (
            <p className="small">
              <span className="success">Verified</span> for a fresh nonce: the TDX quote's report
              data binds the settlement key <code title={attestation.settlement_pubkey}>{short(attestation.settlement_pubkey ?? "")}</code>{" "}
              that signs every webhook ({attestation.quote_bytes ?? 0}-byte quote).
            </p>
          ) : (
            <p className="small danger">The attestation did not bind its keys.</p>
          )}
        </div>
        <div>
          <h3>Application</h3>
          {evidence == null ? (
            <p className="muted small">TLS evidence unavailable.</p>
          ) : (
            <dl className="details">
              <div>
                <dt>App id</dt>
                <dd className="mono">{evidence.app_id}</dd>
              </div>
              {evidence.compose_hash !== undefined && (
                <div>
                  <dt>Compose hash</dt>
                  <dd className="mono" title={evidence.compose_hash}>
                    {short(evidence.compose_hash)}
                  </dd>
                </div>
              )}
            </dl>
          )}
          <p className="muted small">From the TLS certificate evidence quote (at issuance).</p>
        </div>
        <div>
          <h3>Verify it yourself</h3>
          <p className="small">
            <a href={trust?.verify_docs} target="_blank" rel="noreferrer">
              Attestation guide
            </a>{" "}
            ·{" "}
            <a href={trust?.dstack_verifier} target="_blank" rel="noreferrer">
              dstack verifier
            </a>
          </p>
          <p className="muted small">
            Network: {account?.network.name ?? "Sepolia"} {account?.network.testnet === false ? "" : "testnet"}
          </p>
        </div>
      </div>
    </section>
  );
}

function ExplorerLink({
  account,
  kind,
  value,
}: {
  account: Account;
  kind: "address" | "tx";
  value: string;
}) {
  const explorer = account.network.explorer;
  if (explorer === null) {
    return <span className="mono">{short(value)}</span>;
  }
  return (
    <a className="mono" href={`${explorer}/${kind}/${value}`} target="_blank" rel="noreferrer" title={value}>
      {short(value)}
    </a>
  );
}

function stateLabel(state: Step["state"]): string {
  return { complete: "Done", current: "In progress", upcoming: "Pending", failed: "Failed" }[state];
}

function describe(error: unknown): string {
  if (error instanceof ApiError) {
    return error.code === "rate_limited" ? "too many requests, try again in a minute" : error.code;
  }
  return "network error";
}

function usePolling(callback: () => void, interval: number, enabled = true): void {
  useEffect(() => {
    if (!enabled) {
      return;
    }
    callback();
    const timer = setInterval(callback, interval);
    return () => clearInterval(timer);
  }, [callback, interval, enabled]);
}

function useTheme(): [Theme, (theme: Theme) => void] {
  const [theme, setTheme] = useState<Theme>(() => {
    const stored = localStorage.getItem("demo-theme");
    if (stored === "light" || stored === "dark") {
      return stored;
    }
    return window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
  });
  useEffect(() => {
    document.documentElement.dataset["theme"] = theme;
  }, [theme]);
  return [
    theme,
    (next) => {
      localStorage.setItem("demo-theme", next);
      setTheme(next);
    },
  ];
}
