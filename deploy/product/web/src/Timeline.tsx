import { ChevronRight, Circle, CircleCheck, CircleX, LoaderCircle } from "lucide-react";
import type { ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { Card, CardAction, CardContent, CardHeader, CardTitle } from "@/components/ui/card";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { cn } from "@/lib/utils";
import type {
  Account,
  ApiExchange,
  Detail as StepDetail,
  LedgerView,
  Step,
  StepKey,
  Timeline,
  WebhookEvent,
} from "./api.js";
import { Detail, Details, Disclosure, ExplorerLink, Subsection } from "./common.js";
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
      "credit; only a final deposit can be refunded. The time shown is the deposit's final_at.",
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

// A quote's steps, shown before there is a payment to follow.
const PREVIEW: StepKey[] = ["quote_created", "sent", "received", "credited", "webhook_received", "final", "swept"];

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
  // On wide screens it stays in view next to the payment, and scrolls on its own.
  return (
    <Card
      role="complementary"
      aria-labelledby="scenes-title"
      className="min-w-0 xl:sticky xl:top-20 xl:max-h-[calc(100svh-6rem)] xl:overflow-y-auto"
    >
      <CardHeader>
        <CardTitle>
          <h3 id="scenes-title">Behind the scenes</h3>
        </CardTitle>
        {loading !== null && (
          <CardAction>
            <Badge className="bg-success/15 text-success uppercase" aria-hidden="true">
              Live
            </Badge>
          </CardAction>
        )}
      </CardHeader>
      <CardContent className="@container">
        {loading === null ? (
          <div className="flex flex-col gap-5">
            <p className="max-w-prose text-muted-foreground">
              Create a quote, or pay to your deposit address, to follow the payment through the chain,
              the service, and this console's webhook handler, with real data only.
            </p>
            <ol className="hidden max-w-2xl flex-col lg:flex" aria-label="The steps of a payment">
              {PREVIEW.map((key) => (
                <TimelineStep key={key} step={{ key, state: "upcoming", at: null, details: [] }} sent={null} account={account} />
              ))}
            </ol>
          </div>
        ) : timeline === null ? (
          <p className="text-muted-foreground">Loading {short(loading)}…</p>
        ) : (
          <div className="grid gap-6 @4xl:grid-cols-2 @4xl:gap-8">
            <ol className="flex min-w-0 flex-col" aria-label="Payment timeline">
              {timeline.steps.map((step) => (
                <TimelineStep key={step.key} step={step} sent={timeline.sent?.at ?? null} account={account} />
              ))}
            </ol>
            <div className="flex min-w-0 flex-col gap-4">
              {timeline.ledger !== null && <LedgerPanel ledger={timeline.ledger} />}
              <EventsLog events={timeline.events} />
              {timeline.deposit !== null && account !== null && (
                <Refunds timeline={timeline} deposit={timeline.deposit} account={account} onChanged={onChanged} />
              )}
              <DeveloperView exchanges={timeline.api} />
            </div>
          </div>
        )}
      </CardContent>
    </Card>
  );
}

const STEP_ICONS: Record<Step["state"], ReactNode> = {
  complete: <CircleCheck className="size-4.5 text-success" aria-hidden="true" />,
  current: <LoaderCircle className="size-4.5 animate-spin text-foreground" aria-hidden="true" />,
  upcoming: <Circle className="size-4.5 text-muted-foreground/60" aria-hidden="true" />,
  failed: <CircleX className="size-4.5 text-destructive" aria-hidden="true" />,
};

function TimelineStep({ step, sent, account }: { step: Step; sent: number | null; account: Account | null }) {
  const copy = STEP_COPY[step.key];
  const after = step.at !== null && sent !== null && step.key !== "sent" && step.key !== "quote_created";
  return (
    <li
      className="group relative grid grid-cols-[1.125rem_minmax(0,1fr)] gap-3 pb-5 last:pb-0"
      data-step={step.key}
      data-state={step.state}
      aria-current={step.state === "current" ? "step" : undefined}
    >
      <span className="absolute top-6 bottom-1 left-2 w-0.5 rounded-full bg-border group-last:hidden" aria-hidden="true" />
      <span className="mt-px bg-card">{STEP_ICONS[step.state]}</span>
      <div className="flex min-w-0 flex-col gap-0.5">
        <div className="flex items-baseline justify-between gap-2">
          <span className={cn("font-medium", step.state === "upcoming" && "text-muted-foreground")}>
            {copy.title}
          </span>
          <span className="shrink-0 text-xs text-muted-foreground">{stateLabel(step.state)}</span>
        </div>
        {step.at !== null && (
          <div className="text-xs" data-testid="step-time">
            {time(step.at)}
            {after && <span className="text-muted-foreground"> · {duration(step.at - sent)} after sending</span>}
          </div>
        )}
        {step.state === "failed" && copy.failed !== undefined ? (
          <div className="text-xs text-destructive">{copy.failed}</div>
        ) : (
          step.state !== "complete" && <div className="text-xs text-muted-foreground">{copy.hint}</div>
        )}
        {step.details.length > 0 && (
          <Details className="mt-2">
            {step.details.map((detail) => (
              <Detail key={detail.label} label={detail.label}>
                <DetailValue detail={detail} account={account} />
              </Detail>
            ))}
          </Details>
        )}
      </div>
    </li>
  );
}

function DetailValue({ detail, account }: { detail: StepDetail; account: Account | null }) {
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
    return <span className="text-success">{signedDollars(value)}</span>;
  }
  if (kind === "atomic" && typeof value === "string") {
    return <>{tokens(value, account?.token.symbol ?? "")}</>;
  }
  return <span className={detail.mono === true ? "font-mono" : undefined}>{String(value)}</span>;
}

function LedgerPanel({ ledger }: { ledger: LedgerView }) {
  const product = ledger.product;
  return (
    <Subsection title="Ledger" id="ledger-title">
      <p className="text-muted-foreground">
        The balance rule: while <code>credited</code> or <code>reversed</code>, a deposit nets to{" "}
        <code>amount − amount_refunded − amount_reversed</code>, and to 0 otherwise. Every{" "}
        <code>deposit.*</code> event carries those cumulative amounts, so the result does not depend
        on the order events arrive in.
      </p>
      <Details data-testid="ledger">
        <Detail label="Status">{ledger.status}</Detail>
        <Detail label="amount">{ledger.amount === null ? "—" : dollars(ledger.amount)}</Detail>
        <Detail label="amount_refunded">−{dollars(ledger.amount_refunded)}</Detail>
        <Detail label="amount_reversed">−{dollars(ledger.amount_reversed)}</Detail>
        <Detail label="Nets to" data-testid="nets-to">
          <strong>{dollars(ledger.nets_to)}</strong>
        </Detail>
        <Detail label="This console's ledger" data-testid="console-net">
          {product === null || product.status === null
            ? "no order yet"
            : product.net === null
              ? `${product.status}${product.reason === null ? "" : ` (${product.reason})`}`
              : `${dollars(product.net)} (credit ${dollars(product.credit ?? 0)}${product.adjustments
                  .map((adjustment) => `, ${signedDollars(adjustment.amount)} by ${adjustment.reason}`)
                  .join("")})`}
        </Detail>
      </Details>
    </Subsection>
  );
}

function EventsLog({ events }: { events: WebhookEvent[] }) {
  return (
    <Subsection title="Webhook events received" id="events-title">
      {events.length === 0 ? (
        <p className="text-muted-foreground">None yet.</p>
      ) : (
        <Table className="text-xs">
          <TableHeader>
            <TableRow>
              <TableHead scope="col">Type</TableHead>
              <TableHead scope="col">Event id</TableHead>
              <TableHead scope="col">Received</TableHead>
              <TableHead scope="col">Signature</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {events.map((event) => (
              <TableRow key={event.id} data-testid="webhook-event">
                <TableCell>
                  <code>{event.type}</code>
                </TableCell>
                <TableCell className="font-mono" title={event.id}>
                  {short(event.id)}
                </TableCell>
                <TableCell>{time(event.received_at)}</TableCell>
                <TableCell className="text-success">{event.verified ? "Verified" : "—"}</TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </Subsection>
  );
}

export function DeveloperView({ exchanges, title }: { exchanges: ApiExchange[]; title?: string }) {
  return (
    <Disclosure summary={`${title ?? "Developer view: the product's API requests"} (${exchanges.length})`}>
      <p className="text-muted-foreground">
        Sent from the product's server with its restricted API key; the browser never holds it.
      </p>
      {exchanges.map((exchange, index) => (
        <details key={`${exchange.method}-${exchange.url}-${index}`} className="group/exchange">
          <summary className="flex cursor-pointer list-none items-center gap-1 [&::-webkit-details-marker]:hidden">
            <ChevronRight
              className="size-3.5 shrink-0 transition-transform group-open/exchange:rotate-90"
              aria-hidden="true"
            />
            <code className="wrap-anywhere">
              {exchange.method} {new URL(exchange.url).pathname}
              {new URL(exchange.url).search}
            </code>{" "}
            <span className={exchange.status < 400 ? "text-success" : "text-destructive"}>{exchange.status}</span>
          </summary>
          <pre className="mt-2 max-h-80 overflow-auto rounded-lg bg-muted/60 p-3 font-mono text-xs">
            {JSON.stringify({ request: exchange.request, response: exchange.response }, null, 2)}
          </pre>
        </details>
      ))}
    </Disclosure>
  );
}

function stateLabel(state: Step["state"]): string {
  return { complete: "Done", current: "In progress", upcoming: "Pending", failed: "Failed" }[state];
}
