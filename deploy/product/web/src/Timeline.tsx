import type { Account, ApiExchange, Detail, LedgerView, Step, StepKey, Timeline, WebhookEvent } from "./api.js";
import { ExplorerLink } from "./common.js";
import { dollars, duration, short, signedDollars, time, tokens } from "./format.js";
import { Refunds } from "./Refunds.js";

// Each step's title, what it waits for, and the time it usually takes: the hints are expectations,
// every time shown next to them is real (the chain's block time, the service's timestamps, or when
// this console received a webhook).
const STEP_COPY: Record<StepKey, { title: string; hint: string; failed?: string }> = {
  quote_created: {
    title: "Quote created",
    hint: "A locked price for an exact amount, for 15 minutes.",
  },
  sent: {
    title: "Sent on chain",
    hint: "Waiting for a transfer to the address.",
    failed: "The quote expired without a payment.",
  },
  received: {
    title: "Received by Phala Pay",
    hint: "Usually about 12 s after sending: the service scans every new block for its addresses.",
  },
  credited: {
    title: "Credited",
    hint:
      "Usually about 15 s after sending: at 2 confirmations, once both RPC providers report the " +
      "same block, the deposit is valued and screened.",
    failed: "The deposit was rejected and will not be credited.",
  },
  webhook_received: {
    title: "Webhook applied by this console",
    hint: "The signed deposit.credited moves the balance; its metadata arrives with it.",
  },
  final: {
    title: "Final",
    hint:
      "About 15 minutes on Ethereum. Until then a reorg that drops the transaction reverses the " +
      "credit; only a final deposit can be refunded. The time shown is when this demo first saw it final.",
  },
  reversed: {
    title: "Reversed",
    hint: "The transaction left the chain before finality; deposit.reversed took the credit back.",
    failed: "The transaction left the chain before finality; deposit.reversed took the credit back.",
  },
  swept: {
    title: "Swept to the treasury",
    hint:
      "Phala Pay never sweeps: the merchant signs factory.flush from its own wallet or Safe (see " +
      "Sweeps below). The deposit is marked swept once that flush is finalized.",
  },
};

export function BehindTheScenes({
  timeline,
  loading,
  account,
  onChanged,
}: {
  timeline: Timeline | null;
  loading: string | null;
  account: Account | null;
  onChanged: () => void;
}) {
  return (
    <aside className="card scenes" aria-labelledby="scenes-title">
      <div className="scenes-head">
        <h2 id="scenes-title">Behind the scenes</h2>
        {loading !== null && (
          <span className="live" aria-hidden="true">
            Live
          </span>
        )}
      </div>
      {loading === null ? (
        <p className="muted">
          Create a quote, or pay to your deposit address, to follow the payment through the chain,
          the service, and this console's webhook handler, with real data only.
        </p>
      ) : timeline === null ? (
        <p className="muted">Loading {short(loading)}…</p>
      ) : (
        <>
          <ol className="timeline" aria-label="Payment timeline">
            {timeline.steps.map((step) => (
              <TimelineStep key={step.key} step={step} sent={timeline.sent?.at ?? null} account={account} />
            ))}
          </ol>
          {timeline.ledger !== null && <LedgerPanel ledger={timeline.ledger} />}
          {timeline.deposit !== null && account !== null && (
            <Refunds timeline={timeline} deposit={timeline.deposit} account={account} onChanged={onChanged} />
          )}
          <EventsLog events={timeline.events} />
          <DeveloperView exchanges={timeline.api} />
        </>
      )}
    </aside>
  );
}

function TimelineStep({ step, sent, account }: { step: Step; sent: number | null; account: Account | null }) {
  const copy = STEP_COPY[step.key];
  const after = step.at !== null && sent !== null && step.key !== "sent" && step.key !== "quote_created";
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
        {step.at !== null && (
          <div className="small" data-testid="step-time">
            {time(step.at)}
            {after && <span className="muted"> · {duration(step.at - sent)} after sending</span>}
          </div>
        )}
        {step.state === "failed" && copy.failed !== undefined ? (
          <div className="small danger">{copy.failed}</div>
        ) : (
          step.state !== "complete" && <div className="muted small">{copy.hint}</div>
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
  if ((kind === "address" || kind === "tx") && typeof value === "string") {
    return <ExplorerLink account={account} kind={kind} value={value} />;
  }
  if (kind === "time" && typeof value === "number") {
    return <>{time(value)}</>;
  }
  if (kind === "usd" && typeof value === "number") {
    return <>{dollars(value)}</>;
  }
  if (kind === "usd_delta" && typeof value === "number") {
    return <span className="success">{signedDollars(value)}</span>;
  }
  if (kind === "atomic" && typeof value === "string") {
    return <>{tokens(value, account?.token.symbol ?? "")}</>;
  }
  return <span className={detail.mono === true ? "mono" : undefined}>{String(value)}</span>;
}

function LedgerPanel({ ledger }: { ledger: LedgerView }) {
  const product = ledger.product;
  return (
    <section className="subsection" aria-labelledby="ledger-title">
      <h3 id="ledger-title">Ledger</h3>
      <p className="muted small">
        The balance rule: while <code>credited</code> or <code>reversed</code>, a deposit nets to{" "}
        <code>amount − amount_refunded − amount_reversed</code>, and to 0 otherwise. Every{" "}
        <code>deposit.*</code> event carries those cumulative amounts, so the result does not depend
        on the order events arrive in.
      </p>
      <dl className="details" data-testid="ledger">
        <div>
          <dt>Status</dt>
          <dd>{ledger.status}</dd>
        </div>
        <div>
          <dt>amount</dt>
          <dd>{ledger.amount === null ? "—" : dollars(ledger.amount)}</dd>
        </div>
        <div>
          <dt>amount_refunded</dt>
          <dd>−{dollars(ledger.amount_refunded)}</dd>
        </div>
        <div>
          <dt>amount_reversed</dt>
          <dd>−{dollars(ledger.amount_reversed)}</dd>
        </div>
        <div>
          <dt>Nets to</dt>
          <dd data-testid="nets-to">
            <strong>{dollars(ledger.nets_to)}</strong>
          </dd>
        </div>
        <div>
          <dt>This console's ledger</dt>
          <dd data-testid="console-net">
            {product === null || product.status === null
              ? "no order yet"
              : product.net === null
                ? `${product.status}${product.reason === null ? "" : ` (${product.reason})`}`
                : `${dollars(product.net)} (credit ${dollars(product.credit ?? 0)}${product.adjustments
                    .map((adjustment) => `, ${signedDollars(adjustment.amount)} by ${adjustment.reason}`)
                    .join("")})`}
          </dd>
        </div>
      </dl>
    </section>
  );
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

export function DeveloperView({ exchanges, title }: { exchanges: ApiExchange[]; title?: string }) {
  return (
    <details className="subsection dev">
      <summary>
        {title ?? "Developer view: the product's API requests"} ({exchanges.length})
      </summary>
      <p className="muted small">
        Sent from the product's server with its restricted API key; the browser never holds it.
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

function stateLabel(state: Step["state"]): string {
  return { complete: "Done", current: "In progress", upcoming: "Pending", failed: "Failed" }[state];
}
