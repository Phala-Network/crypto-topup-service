import { CircleAlert } from "lucide-react";
import { useId, useState, type FormEvent } from "react";
import { isAddress, parseUnits } from "viem";
import { Alert, AlertDescription } from "@/components/ui/alert";
import { Button } from "@/components/ui/button";
import { Field, FieldLabel } from "@/components/ui/field";
import { Input } from "@/components/ui/input";
import {
  cancelRefund,
  createRefund,
  markRefundPaid,
  type Account,
  type Deposit,
  type Refund,
  type Timeline,
} from "./api.js";
import { Detail, Details, ExplorerLink, InfoTip, StatusBadge, Subsection, describe, errorMessage, wallet } from "./common.js";
import { statusLabel, tokens } from "./format.js";

/**
 * The refund flow (design D5): declare the refund, pay it from the treasury the deposit's address
 * pays, attach the transaction; the service verifies it at finality. On staging the product holds
 * no keys and the treasury is Phala's finance Safe, so the visitor plays the merchant's finance
 * team: a payment from the treasury succeeds, and one from any other wallet fails verification.
 */
export function Refunds({
  timeline,
  deposit,
  account,
  onChanged,
}: {
  timeline: Timeline;
  deposit: Deposit;
  account: Account;
  onChanged: () => void;
}) {
  const symbol = account.token.symbol;
  const refundable = deposit.final && (deposit.status === "credited" || deposit.status === "rejected");
  return (
    <Subsection
      title={`Refunds (${timeline.refunds.length})`}
      id="refunds-title"
      aside={
        <InfoTip label="About refunds">
          The merchant refunds from its own treasury: declare the refund, pay it from the treasury that this
          deposit's address pays, then attach the transaction. Phala Pay verifies it once the transaction is final
          and never moves funds. Here you play the merchant's finance team.
        </InfoTip>
      }
    >
      <p className="text-muted-foreground">
        On this staging demo the treasury{" "}
        <ExplorerLink account={account} kind="address" value={account.treasury} /> is Phala's finance Safe, which
        you do not control: a refund you pay from your own wallet is verified and <strong>fails</strong> with{" "}
        <code>sender_mismatch</code>, which is exactly what should happen.
      </p>
      {refundable ? (
        <RefundForm deposit={deposit} symbol={symbol} onCreated={onChanged} />
      ) : (
        <p data-testid="refund-unavailable" className="text-muted-foreground">
          {deposit.status === "reversed"
            ? "A reversed deposit cannot be refunded."
            : "Refunds need a final deposit (the service answers 400 deposit_not_final before)."}
        </p>
      )}
      {timeline.refunds.length > 0 && (
        <ul className="flex flex-col gap-3" aria-label="Refunds of this deposit">
          {timeline.refunds.map((refund) => (
            <RefundItem key={refund.id} refund={refund} account={account} onChanged={onChanged} />
          ))}
        </ul>
      )}
    </Subsection>
  );
}

function RefundForm({ deposit, symbol, onCreated }: { deposit: Deposit; symbol: string; onCreated: () => void }) {
  const remaining = BigInt(deposit.amount_atomic) - BigInt(deposit.amount_refunded_atomic);
  const [amount, setAmount] = useState("");
  const [destination, setDestination] = useState(deposit.from_address);
  const [state, setState] = useState<{ pending: boolean; error: string | null }>({ pending: false, error: null });
  const amountId = useId();
  const destinationId = useId();
  const submit = (event: FormEvent) => {
    event.preventDefault();
    let atomic: bigint;
    try {
      atomic = parseUnits(amount.trim(), 18);
    } catch {
      setState({ pending: false, error: `Enter an amount of ${symbol}.` });
      return;
    }
    if (atomic <= 0n || atomic > remaining) {
      setState({ pending: false, error: `Enter at most ${tokens(remaining.toString(), symbol)}.` });
      return;
    }
    if (!isAddress(destination)) {
      setState({ pending: false, error: "Enter a 0x address for the destination." });
      return;
    }
    setState({ pending: true, error: null });
    createRefund(deposit.id, atomic.toString(), destination).then(
      () => {
        setState({ pending: false, error: null });
        setAmount("");
        onCreated();
      },
      (error: unknown) => setState({ pending: false, error: `Could not declare the refund: ${describe(error)}.` }),
    );
  };
  return (
    <form className="grid gap-3 sm:grid-cols-[minmax(0,1fr)_minmax(0,1.6fr)]" onSubmit={submit} aria-label="Declare a refund">
      <Field>
        <FieldLabel htmlFor={amountId}>
          Amount ({symbol}, at most {tokens(remaining.toString(), symbol)})
        </FieldLabel>
        <Input
          id={amountId}
          inputMode="decimal"
          placeholder="10"
          value={amount}
          onChange={(event) => setAmount(event.target.value)}
        />
      </Field>
      <Field>
        <FieldLabel htmlFor={destinationId}>Destination address (the payer's, by default)</FieldLabel>
        <Input
          id={destinationId}
          className="font-mono text-xs md:text-xs"
          spellCheck={false}
          value={destination}
          onChange={(event) => setDestination(event.target.value)}
        />
      </Field>
      <Button type="submit" variant="outline" className="self-start sm:col-span-2" disabled={state.pending}>
        {state.pending ? "Declaring…" : "Declare refund"}
      </Button>
      {state.error !== null && (
        <div className="sm:col-span-2">
          <ErrorAlert text={state.error} />
        </div>
      )}
    </form>
  );
}

function RefundItem({ refund, account, onChanged }: { refund: Refund; account: Account; onChanged: () => void }) {
  const symbol = account.token.symbol;
  const [hash, setHash] = useState("");
  const [logIndex, setLogIndex] = useState("");
  const [state, setState] = useState<{ pending: string | null; error: string | null }>({ pending: null, error: null });
  const hashId = useId();
  const indexId = useId();
  const run = (label: string, action: () => Promise<unknown>) => {
    setState({ pending: label, error: null });
    action().then(
      () => {
        setState({ pending: null, error: null });
        onChanged();
      },
      (error: unknown) => setState({ pending: null, error: describe(error) }),
    );
  };
  const markPaid = (event: FormEvent) => {
    event.preventDefault();
    const trimmed = hash.trim();
    if (!/^0x[0-9a-fA-F]{64}$/.test(trimmed)) {
      setState({ pending: null, error: "Enter the 0x transaction hash of the payment." });
      return;
    }
    const index = logIndex.trim() === "" ? null : Number(logIndex);
    if (index !== null && !(Number.isSafeInteger(index) && index >= 0)) {
      setState({ pending: null, error: "The receipt log index is a whole number." });
      return;
    }
    run("mark", () => markRefundPaid(refund.id, trimmed, index));
  };
  const transfer = refund.transfer;
  return (
    <li
      className="flex flex-col gap-3 rounded-lg border bg-card p-4"
      data-testid="refund"
      data-refund={refund.id}
      data-status={refund.status}
    >
      <div className="flex items-center justify-between gap-2">
        <span className="font-mono text-muted-foreground" title={refund.id}>
          {refund.id.slice(0, 11)}…
        </span>
        <StatusBadge status={refund.status}>{statusLabel(refund.status)}</StatusBadge>
      </div>
      <p>
        {tokens(refund.amount_atomic, symbol)} to{" "}
        <ExplorerLink account={account} kind="address" value={refund.destination_address} />
      </p>
      {transfer !== null && (
        <>
          <div className="flex flex-col gap-2" data-testid="refund-transfer">
            <p>
              <strong>Pay exactly this transfer from the treasury</strong>, then attach its hash:
            </p>
            <Details>
              <Detail label="From (treasury)" className="font-mono">
                {transfer.from}
              </Detail>
              <Detail label="Token contract" className="font-mono">
                {transfer.token}
              </Detail>
              <Detail label="To" className="font-mono">
                {transfer.to}
              </Detail>
              <Detail label="Amount">
                {tokens(transfer.amount_atomic, symbol)} (<span className="font-mono">{transfer.amount_atomic}</span>)
              </Detail>
              <Detail label="Calldata" className="font-mono">
                {transfer.data}
              </Detail>
            </Details>
          </div>
          <form className="flex flex-col gap-3" onSubmit={markPaid} aria-label={`Mark refund ${refund.id} paid`}>
            <Field>
              <FieldLabel htmlFor={hashId}>Transaction hash of the payment</FieldLabel>
              <Input
                id={hashId}
                className="font-mono text-xs md:text-xs"
                spellCheck={false}
                placeholder="0x…"
                value={hash}
                onChange={(event) => setHash(event.target.value)}
              />
            </Field>
            <Field>
              <FieldLabel htmlFor={indexId}>
                Receipt log index (optional, when one transaction pays several refunds)
              </FieldLabel>
              <Input
                id={indexId}
                inputMode="numeric"
                value={logIndex}
                onChange={(event) => setLogIndex(event.target.value)}
              />
            </Field>
            <div className="flex flex-wrap gap-2">
              <Button type="submit" variant="outline" disabled={state.pending !== null}>
                {state.pending === "mark" ? "Submitting…" : "Mark paid"}
              </Button>
              <Button
                type="button"
                variant="ghost"
                disabled={state.pending !== null}
                onClick={() => run("cancel", () => cancelRefund(refund.id))}
              >
                {state.pending === "cancel" ? "Canceling…" : "Cancel refund"}
              </Button>
            </div>
          </form>
          <p className="text-muted-foreground">
            Not the treasury?{" "}
            <Button
              type="button"
              variant="link"
              className="h-auto p-0 text-xs text-foreground underline"
              disabled={state.pending !== null}
              onClick={() => {
                setState({ pending: "wallet", error: null });
                wallet()
                  .then(({ transferTokens }) =>
                    transferTokens(account.network.chain_id, transfer.token, transfer.to, BigInt(transfer.amount_atomic)),
                  )
                  .then(
                    (sent) => {
                      setHash(sent);
                      setState({ pending: null, error: null });
                    },
                    (error: unknown) =>
                      setState({ pending: null, error: errorMessage(error, "The wallet did not send it.") }),
                  );
              }}
            >
              {state.pending === "wallet" ? "Confirm in your wallet…" : "Pay it from my wallet instead"}
            </Button>{" "}
            and mark that transaction paid to see verification fail.
          </p>
        </>
      )}
      {refund.status === "pending" && refund.transaction_hash !== null && (
        <p role="status">
          Marked paid with <ExplorerLink account={account} kind="tx" value={refund.transaction_hash} />. Verifying once
          the transaction is final (about 15 minutes on Sepolia); the amount stays reserved meanwhile.
        </p>
      )}
      {refund.status === "succeeded" && (
        <p className="text-success" role="status">
          Succeeded: the service verified the treasury's transfer at finality and sent{" "}
          <code>deposit.refunded</code>, which took the refunded share back from the balance.
        </p>
      )}
      {refund.status === "failed" && (
        <p className="text-destructive" role="status">
          Failed: <code>{refund.failure_reason}</code>. {refund.failure_explanation} Its reservation of the
          deposit is released; declare a new refund and pay it from the treasury.
        </p>
      )}
      {refund.status === "canceled" && <p className="text-muted-foreground">Canceled before any payment was attached.</p>}
      {state.error !== null && <ErrorAlert text={state.error} />}
    </li>
  );
}

function ErrorAlert({ text }: { text: string }) {
  return (
    <Alert variant="destructive">
      <CircleAlert aria-hidden="true" />
      <AlertDescription className="text-xs">{text}</AlertDescription>
    </Alert>
  );
}
