import { Checkout, type Appearance } from "@phala/pay/react";
import { useCallback, useEffect, useId, useRef, useState, type FormEvent, type KeyboardEvent } from "react";
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
import { ExplorerLink, describe, usePolling } from "./common.js";
import { DepositAddressPanel } from "./DepositAddressPanel.js";
import { dollars, short, signedDollars, statusLabel, time, tokens } from "./format.js";
import { Sweeps } from "./Sweeps.js";
import { errorMessage, mintTestTokens } from "./testTokens.js";
import { BehindTheScenes } from "./Timeline.js";

type Theme = "light" | "dark";
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
  const select = (next: Selection) => {
    setSelected(next);
    setTimeline(null);
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
            A cloud console's billing page paid with Phala Pay: top up this workspace with{" "}
            {account?.token.symbol ?? "PHA"} on {account?.network.name ?? "Sepolia"}, either for an
            exact amount at a locked price, or at any time to your own deposit address. The balance
            moves only when this console's webhook handler receives a verified <code>deposit.*</code>{" "}
            event, exactly as a real integration applies credits.
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
              <MethodTabs method={method} onChange={setMethod} />
              <div
                role="tabpanel"
                id={`panel-${method}`}
                aria-labelledby={`tab-${method}`}
                className="tabpanel"
              >
                {method === "quote" ? (
                  session === null || account === null ? (
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
                    <div className="checkout">
                      <p className="small">
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
                      <button type="button" className="ghost" onClick={() => setSession(null)}>
                        Start a new top-up
                      </button>
                    </div>
                  )
                ) : account === null ? (
                  <p className="muted">Loading…</p>
                ) : (
                  <DepositAddressPanel account={account} appearance={appearance} onSelect={select} />
                )}
              </div>
            </section>
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
  );
}

function MethodTabs({ method, onChange }: { method: Method; onChange: (method: Method) => void }) {
  const refs = useRef<Record<Method, HTMLButtonElement | null>>({ quote: null, address: null });
  // Arrow keys move between tabs (WAI-ARIA tabs pattern, automatic activation).
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key !== "ArrowLeft" && event.key !== "ArrowRight") {
      return;
    }
    event.preventDefault();
    const next = method === "quote" ? "address" : "quote";
    onChange(next);
    refs.current[next]?.focus();
  };
  return (
    <div className="tabs" role="tablist" aria-label="Payment method">
      {METHODS.map(({ id, label }) => (
        <button
          key={id}
          ref={(element) => {
            refs.current[id] = element;
          }}
          id={`tab-${id}`}
          type="button"
          role="tab"
          className="tab"
          aria-selected={method === id}
          aria-controls={`panel-${id}`}
          tabIndex={method === id ? 0 : -1}
          onClick={() => onChange(id)}
          onKeyDown={onKeyDown}
        >
          {label}
        </button>
      ))}
    </div>
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
        Workspace <code>{account?.account_id ?? "…"}</code>, a demo account kept in a cookie in this
        browser.
      </p>
      {account !== null && account.ledger.length > 0 && (
        <details className="ledger-lines">
          <summary>How this balance adds up ({account.ledger.length})</summary>
          <div className="table-scroll">
          <table className="table compact">
            <thead>
              <tr>
                <th scope="col">When</th>
                <th scope="col">Deposit</th>
                <th scope="col">Event</th>
                <th scope="col">Amount</th>
              </tr>
            </thead>
            <tbody>
              {account.ledger.map((line) => (
                <tr key={`${line.deposit}-${line.reason}-${line.at}`} data-testid="ledger-line">
                  <td>{time(line.at)}</td>
                  <td className="mono" title={line.deposit}>
                    {short(line.deposit)}
                  </td>
                  <td>
                    <code>{line.reason}</code>
                  </td>
                  <td className={line.amount < 0 ? "danger" : "success"}>{signedDollars(line.amount)}</td>
                </tr>
              ))}
            </tbody>
          </table>
          </div>
        </details>
      )}
    </section>
  );
}

function AmountPicker({ account, onQuote }: { account: Account | null; onQuote: (created: CreatedQuote) => void }) {
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
              <input type="radio" name="amount" checked={preset === cents} onChange={() => setPreset(cents)} />
              <span>{dollars(cents)}</span>
            </label>
          ))}
          <label className="preset">
            <input type="radio" name="amount" checked={preset === "custom"} onChange={() => setPreset("custom")} />
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
        A quote locks the price for 15 minutes for an exact amount. Pay from a browser wallet, by QR
        code, or by sending the exact amount manually; another amount, or a late payment, is credited
        at the market rate instead.
      </p>
      {error !== null && (
        <p className="alert" role="alert">
          {error}
        </p>
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
    <section className="card" aria-labelledby="history-title">
      <h2 id="history-title">Payments</h2>
      {account === null || account.payments.length === 0 ? (
        <p className="muted">No top-ups yet.</p>
      ) : (
        <div className="table-scroll">
          <table className="table">
            <thead>
              <tr>
                <th scope="col">Date</th>
                <th scope="col">Method</th>
                <th scope="col">{symbol}</th>
                <th scope="col">Transaction</th>
                <th scope="col">Status</th>
                <th scope="col">Credited</th>
                <th scope="col">Refunded</th>
                <th scope="col">Nets to</th>
                <th scope="col">
                  <span className="visually-hidden">Timeline</span>
                </th>
              </tr>
            </thead>
            <tbody>
              {account.payments.map((row) => {
                const selection: Selection = row.id.startsWith("dep_")
                  ? { kind: "deposit", id: row.id }
                  : { kind: "quote", id: row.id };
                return (
                  <tr
                    key={row.id}
                    data-testid="payment"
                    data-kind={row.kind}
                    aria-selected={selected?.id === row.id || selected?.id === row.quote}
                  >
                    <td>{time(row.created)}</td>
                    <td>{row.kind === "quote" ? "Quote" : "Deposit address"}</td>
                    <td>{tokens(row.amount_atomic, symbol)}</td>
                    <td>{row.tx_hash === null ? "—" : <ExplorerLink account={account} kind="tx" value={row.tx_hash} />}</td>
                    <td>
                      <span className={`badge ${row.status}`}>{statusLabel(row.status)}</span>
                      {row.final && <span className="badge"> final</span>}
                      {row.swept && <span className="badge"> swept</span>}
                    </td>
                    <td>{row.amount === null || row.tx_hash === null ? "—" : dollars(row.amount)}</td>
                    <td>{row.amount_refunded_atomic === "0" ? "—" : tokens(row.amount_refunded_atomic, symbol)}</td>
                    <td>{row.net === null ? "—" : dollars(row.net)}</td>
                    <td>
                      <button
                        type="button"
                        className="link"
                        onClick={() => onSelect(selection)}
                        aria-label={`Timeline of ${row.id}`}
                      >
                        Timeline
                      </button>
                    </td>
                  </tr>
                );
              })}
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
              data binds this account's webhook key{" "}
              <code title={attestation.webhook_public_key}>{short(attestation.webhook_public_key ?? "")}</code> that
              signs every webhook ({attestation.quote_bytes ?? 0}-byte quote).
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
          <h3>Non-custodial</h3>
          <p className="small">
            Every address pays only the merchant's treasury, fixed in the address. Phala Pay holds no
            funds and sends no transactions: the merchant sweeps and refunds itself.
          </p>
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
