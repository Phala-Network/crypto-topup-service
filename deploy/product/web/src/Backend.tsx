import { Cpu, ShieldCheck, Terminal, Wallet } from "lucide-react";
import type { ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { cn } from "@/lib/utils";
import type { Account, DepositAddressResponse, Network, Selection, Timeline, Trust } from "./api.js";
import { CopyButton, Detail, Details, Empty, ExplorerLink, InfoTip, LINK, StatusBadge, Subsection } from "./common.js";
import { AreaLabel } from "./Product.js";
import { Refunds } from "./Refunds.js";
import { assetOf, networkOf } from "./chains.js";
import { day, dollars, price, short, signedDollars, statusLabel, time, tokenName, tokens } from "./format.js";
import { Sweeps } from "./Sweeps.js";
import { EventStream, EventsLog, LedgerPanel, Requests } from "./Timeline.js";

/**
 * What the merchant's backend sees while its customer pays: the payment's live steps, then its
 * payments, refunds, sweeps, API requests, and the service's attestation. A console in either
 * theme, apart from the product's own surface.
 */
// Every table of the panel: one cell padding, so their columns share a left edge.
const TABLE = "text-xs [&_td]:px-3 [&_th]:h-9 [&_th]:px-3 [&_th]:text-muted-foreground";
// The shown payment's row: highlighted, with an indicator on its left edge.
const SELECTED_ROW = "data-[state=selected]:bg-muted data-[state=selected]:shadow-[inset_2px_0_0_var(--primary)]";

export function Backend({
  account,
  selected,
  timeline,
  trust,
  address,
  networks,
  onSelect,
}: {
  account: Account | null;
  selected: Selection | null;
  timeline: Timeline | null;
  trust: Trust | null;
  address: DepositAddressResponse | null;
  networks: Network[] | undefined;
  onSelect: (selection: Selection) => void;
}) {
  const live = timeline?.steps.some((step) => step.state === "current") ?? selected !== null;
  const order = timeline?.quote?.metadata["order_id"];
  const deposit = timeline?.deposit ?? null;
  return (
    // At lg and up the console follows the scroll beside the product, whichever is taller.
    <div className="flex min-w-0 flex-col gap-3 lg:sticky lg:top-20 lg:self-start">
      <AreaLabel icon={<Terminal />} title="Behind the scenes" text="What your backend sees" />
      <aside
        aria-label="Behind the scenes"
        className="@container/console flex min-w-0 flex-col overflow-hidden rounded-xl border bg-card text-card-foreground shadow-sm"
      >
        <header className="flex flex-wrap items-center gap-x-3 gap-y-2 border-b px-5 py-3.5">
          <h2 className="text-sm font-medium">Event stream</h2>
          <Badge variant="outline" className="gap-1.5 font-normal text-muted-foreground" data-testid="stream-status">
            <span className="relative flex size-2" aria-hidden="true">
              {live && <span className="absolute inset-0 rounded-full bg-success/60 motion-safe:animate-ping" />}
              <span className={cn("relative size-2 rounded-full", live ? "bg-success" : "bg-muted-foreground/50")} />
            </span>
            {selected === null ? "Idle" : live ? "Live" : "Done"}
          </Badge>
          {selected !== null && (
            <dl className="flex flex-wrap items-center gap-x-4 gap-y-1 text-xs @2xl/console:ml-auto">
              {order !== undefined && <MetaItem label="Order" value={order} testId="meta-order" />}
              <MetaItem label={selected.kind === "quote" ? "Quote" : "Deposit"} value={selected.id} testId="meta-selected" />
            </dl>
          )}
        </header>
        <div className="px-3 py-3" aria-live="off">
          <EventStream timeline={timeline} loading={selected?.id ?? null} />
        </div>
        <Tabs defaultValue="credits" className="gap-0 border-t">
          <TabsList
            variant="line"
            aria-label="Backend"
            className="h-11! w-full justify-start gap-2 overflow-x-auto rounded-none border-b px-3.5 py-0 sm:gap-5 sm:px-5"
          >
            <Tab value="credits" count={account?.payments.length}>
              Credits
            </Tab>
            <Tab value="refunds" count={timeline?.refunds.length}>
              Refunds
            </Tab>
            <Tab value="sweeps">Sweeps</Tab>
            <Tab value="api" count={timeline === null ? undefined : timeline.api.length + timeline.events.length}>
              API
            </Tab>
            <Tab value="trust">Trust</Tab>
          </TabsList>
          <TabsContent value="credits" className="p-5">
            <CreditsTab
              account={account}
              selected={selected}
              address={address}
              networks={networks}
              onSelect={onSelect}
            />
          </TabsContent>
          <TabsContent value="refunds" className="p-5">
            {timeline === null || deposit === null || account === null ? (
              <Empty>Follow a payment with a deposit to see its ledger and refunds.</Empty>
            ) : (
              <div className="grid gap-8 @4xl/console:grid-cols-[minmax(0,1fr)_minmax(0,1.3fr)]">
                {timeline.ledger !== null && <LedgerPanel ledger={timeline.ledger} />}
                <Refunds timeline={timeline} deposit={deposit} />
              </div>
            )}
          </TabsContent>
          <TabsContent value="sweeps" className="p-5">
            <Sweeps />
          </TabsContent>
          <TabsContent value="api" className="p-5">
            {timeline === null ? (
              <Empty>Follow a payment to see its webhooks and the product's API requests.</Empty>
            ) : (
              <div className="flex flex-col gap-8">
                <EventsLog events={timeline.events} />
                <Requests exchanges={timeline.api} title="API requests" id="api-title" />
              </div>
            )}
          </TabsContent>
          <TabsContent value="trust" className="p-5">
            <TrustDetails trust={trust} networks={networks} />
          </TabsContent>
        </Tabs>
      </aside>
    </div>
  );
}

/** A key and its value in the stream's header, with a copy button. */
function MetaItem({ label, value, testId }: { label: string; value: string; testId: string }) {
  return (
    <div className="flex items-center gap-1.5">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="flex items-center gap-1 font-mono" title={value} data-testid={testId}>
        {short(value)}
        <CopyButton value={value} label={`Copy ${label.toLowerCase()} id`} />
      </dd>
    </div>
  );
}

function Tab({ value, count, children }: { value: string; count?: number | undefined; children: ReactNode }) {
  return (
    <TabsTrigger value={value} className="h-full flex-none px-0 text-sm after:bottom-[-1px]!">
      {children}
      {count !== undefined && count > 0 && (
        <span className="rounded-full bg-muted px-1.5 font-mono text-xs text-muted-foreground tabular-nums">
          {count}
        </span>
      )}
    </TabsTrigger>
  );
}

/** Shows a payment in the event stream above; the shown one's row is highlighted. */
function ViewButton({ selected, id, onClick }: { selected: boolean; id: string; onClick: () => void }) {
  return (
    <Button
      type="button"
      variant="link"
      size="xs"
      className="h-auto px-0 text-xs aria-pressed:text-muted-foreground aria-pressed:no-underline"
      onClick={onClick}
      aria-label={`View ${id}`}
      aria-pressed={selected}
    >
      {selected ? "Viewing" : "View"}
    </Button>
  );
}

function CreditsTab({
  account,
  selected,
  address,
  networks,
  onSelect,
}: {
  account: Account | null;
  selected: Selection | null;
  address: DepositAddressResponse | null;
  networks: Network[] | undefined;
  onSelect: (selection: Selection) => void;
}) {
  // One empty state for the panel until the first payment.
  if (account === null || (account.payments.length === 0 && account.ledger.length === 0 && address === null)) {
    return <Empty>No credits yet. Pay with crypto in the product to follow a payment here.</Empty>;
  }
  return (
    <div className="flex flex-col gap-8">
      {account.payments.length > 0 && (
        <section aria-label="Credits" className="flex min-w-0 flex-col text-xs">
          <Table className={cn(TABLE, "[&_td]:align-top [&_td]:leading-5")}>
            <TableHeader>
              <TableRow className="hover:bg-transparent">
                <TableHead scope="col">Payment</TableHead>
                <TableHead scope="col">Amount</TableHead>
                <TableHead scope="col">Status</TableHead>
                <TableHead scope="col" className="text-right">
                  Credited
                </TableHead>
                <TableHead scope="col" className="text-right">
                  Nets to
                </TableHead>
                <TableHead scope="col" className="text-right">
                  Timeline
                </TableHead>
              </TableRow>
            </TableHeader>
            <TableBody className="tabular-nums">
              {account.payments.map((row) => {
                const selection: Selection = row.id.startsWith("dep_")
                  ? { kind: "deposit", id: row.id }
                  : { kind: "quote", id: row.id };
                const isSelected = selected?.id === row.id || selected?.id === row.quote;
                const network = networkOf(networks, row.chain_id);
                const decimals = assetOf(network, row.asset)?.decimals;
                const symbol = (row.asset ?? "token").toUpperCase();
                return (
                  <TableRow
                    key={row.id}
                    data-testid="payment"
                    data-kind={row.kind}
                    data-state={isSelected ? "selected" : undefined}
                    aria-selected={isSelected}
                    className={SELECTED_ROW}
                  >
                    <TableCell>
                      <div className="font-medium">{row.kind === "quote" ? "Quote" : "Deposit address"}</div>
                      <div className="text-muted-foreground" title={time(row.created)}>
                        {day(row.created)}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div>{tokens(row.amount_atomic, symbol, decimals)}</div>
                      <div className="text-muted-foreground">
                        {row.exchange_rate === null ? "—" : `at ${price(row.exchange_rate)} / ${symbol}`}
                      </div>
                    </TableCell>
                    <TableCell>
                      <div className="flex max-w-40 flex-wrap gap-1">
                        <StatusBadge status={row.status}>{statusLabel(row.status)}</StatusBadge>
                        {row.final && <Badge variant="outline">final</Badge>}
                        {row.swept && <StatusBadge status="swept">swept</StatusBadge>}
                      </div>
                      {row.tx_hash !== null && (
                        <div className="mt-1">
                          <ExplorerLink chainId={network?.chain_id} kind="tx" value={row.tx_hash} />
                        </div>
                      )}
                    </TableCell>
                    <TableCell className="text-right">
                      <div>{row.amount === null || row.tx_hash === null ? "—" : dollars(row.amount)}</div>
                      {row.bonus !== null && row.bonus !== 0 && (
                        <div className="text-muted-foreground">{signedDollars(row.bonus)} bonus</div>
                      )}
                    </TableCell>
                    <TableCell className="text-right">
                      <div className="font-medium">{row.net === null ? "—" : dollars(row.net)}</div>
                      {row.amount_refunded_atomic !== "0" && (
                        <div className="text-muted-foreground">
                          −{tokens(row.amount_refunded_atomic, symbol, decimals)} refunded
                        </div>
                      )}
                    </TableCell>
                    <TableCell className="text-right">
                      <ViewButton selected={isSelected} id={row.id} onClick={() => onSelect(selection)} />
                    </TableCell>
                  </TableRow>
                );
              })}
            </TableBody>
          </Table>
        </section>
      )}
      <div className={cn("grid gap-8", address !== null && "@4xl/console:grid-cols-2")}>
        {account.ledger.length > 0 && (
          <Subsection
            title="How this balance adds up"
            id="balance-lines-title"
            aside={
              <InfoTip label="About the bonus lines">
                A bonus is this demo merchant's own promotion, not a Phala Pay feature: its backend adds a line of its own
                on deposit.credited, and takes the same share back when a refund or reversal nets the credit down.
              </InfoTip>
            }
          >
            <Table className={TABLE}>
              <TableHeader>
                <TableRow className="hover:bg-transparent">
                  <TableHead scope="col">When</TableHead>
                  <TableHead scope="col">Deposit</TableHead>
                  <TableHead scope="col">Event</TableHead>
                  <TableHead scope="col" className="text-right">
                    Amount
                  </TableHead>
                </TableRow>
              </TableHeader>
              <TableBody className="tabular-nums">
                {account.ledger.map((line) => (
                  <TableRow
                    key={`${line.deposit}-${line.kind}-${line.reason}-${line.at}`}
                    data-testid="ledger-line"
                    data-kind={line.kind}
                  >
                    <TableCell className="text-muted-foreground" title={time(line.at)}>
                      {day(line.at)}
                    </TableCell>
                    <TableCell className="font-mono" title={line.deposit}>
                      {short(line.deposit)}
                    </TableCell>
                    <TableCell>
                      <span className="flex flex-wrap items-center gap-1.5">
                        {line.kind === "bonus" && <Badge variant="outline">Bonus</Badge>}
                        {/* Event names in mono; a bonus grant's label is prose. */}
                        <span className={cn("text-muted-foreground", line.reason.startsWith("deposit.") && "font-mono")}>
                          {line.reason}
                        </span>
                      </span>
                    </TableCell>
                    <TableCell className={cn("text-right", line.amount < 0 ? "text-destructive" : "text-success")}>
                      {signedDollars(line.amount)}
                    </TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          </Subsection>
        )}
        {address !== null && (
          <AddressView
            account={account}
            address={address}
            networks={networks}
            selected={selected}
            onSelect={onSelect}
          />
        )}
      </div>
    </div>
  );
}

/** The deposit address as the product sees it: checked against its pins, and its payments. */
function AddressView({
  account,
  address,
  networks,
  selected,
  onSelect,
}: {
  account: Account;
  address: DepositAddressResponse;
  networks: Network[] | undefined;
  selected: Selection | null;
  onSelect: (selection: Selection) => void;
}) {
  const view = address.deposit_address;
  // Each of the address's networks as the product's selectors name it and its tokens.
  const named = (chainId: number) => networks?.find((each) => each.chain_id === chainId);
  return (
    <Subsection
      title="Deposit address"
      id="address-title"
      aside={
        address.verified ? (
          <Badge className="bg-success/15 text-success" data-testid="deposit-address-verified">
            <ShieldCheck aria-hidden="true" />
            Verified
            <InfoTip label="About the address check" className="text-success">
              The product's SDK recomputed {view.address === null ? "every network's address" : "this address"} from
              its pinned account, factory, implementation, and treasury before showing it.
            </InfoTip>
          </Badge>
        ) : (
          <Badge variant="destructive">Not verified</Badge>
        )
      }
    >
      <Details>
        {view.address !== null ? (
          <Detail label="Address (every network)" className="font-mono" data-testid="deposit-address">
            {view.address}
          </Detail>
        ) : (
          view.networks.map((network) => (
            <Detail
              key={network.chain_id}
              label={`Address on ${named(network.chain_id)?.name ?? `chain ${network.chain_id}`}`}
              className="font-mono"
            >
              {network.address}
            </Detail>
          ))
        )}
        {view.networks.map((network) => {
          const known = named(network.chain_id);
          return (
            <Detail
              key={network.chain_id}
              label={known?.name ?? `Chain ${network.chain_id}`}
              data-testid="deposit-address-network"
            >
              {network.assets
                .map((asset) => tokenName(asset.asset.toUpperCase(), known?.testnet ?? true))
                .join(", ")}
            </Detail>
          );
        })}
        <Detail label="Metadata" className="font-mono">
          {JSON.stringify(view.metadata)}
        </Detail>
      </Details>
      {view.payments.length === 0 ? (
        <Empty>No payments yet. Send any amount of a supported token to the address.</Empty>
      ) : (
        <ul className="flex flex-col divide-y rounded-lg border" aria-label="Payments the product sees" aria-live="polite">
          {view.payments.map((payment) => {
            // The deposit's valuation, once the service recorded it.
            const rate = account.payments.find((row) => row.id === payment.deposit)?.exchange_rate ?? null;
            const token = assetOf(named(payment.chain_id), payment.asset);
            const symbol = (payment.asset ?? "token").toUpperCase();
            return (
              <li
                key={payment.deposit}
                data-testid="address-payment"
                data-state={selected?.id === payment.deposit ? "selected" : undefined}
                className={cn("flex items-center gap-3 px-3 py-2", SELECTED_ROW)}
              >
                <span className="flex min-w-0 flex-1 flex-col items-start gap-0.5">
                  <span>
                    <span className="font-medium tabular-nums">
                      {tokens(payment.amount_atomic, symbol, token?.decimals)}
                    </span>{" "}
                    <span className="text-muted-foreground">
                      {payment.status === "seen"
                        ? `received, ${payment.confirmations ?? 0} confirmation${payment.confirmations === 1 ? "" : "s"}`
                        : rate === null
                          ? "recorded as a deposit"
                          : `credited at ${price(rate)} / ${symbol}`}
                    </span>
                  </span>
                  <ExplorerLink chainId={payment.chain_id} kind="tx" value={payment.tx_hash} />
                </span>
                <ViewButton
                  selected={selected?.id === payment.deposit}
                  id={payment.deposit}
                  onClick={() => onSelect({ kind: "deposit", id: payment.deposit })}
                />
              </li>
            );
          })}
        </ul>
      )}
    </Subsection>
  );
}

function TrustDetails({ trust, networks }: { trust: Trust | null; networks: Network[] | undefined }) {
  const attestation = trust?.attestation;
  const evidence = trust?.tls_evidence;
  return (
    <div className="flex flex-col gap-6 text-xs">
      <div className="flex flex-wrap items-center gap-3">
        <h3 className="text-sm font-medium">Why you can trust Phala Pay</h3>
        {attestation?.binding_verified === true && (
          <Badge className="bg-success/15 text-success">Attestation verified</Badge>
        )}
        <span className="ml-auto flex gap-4">
          <a className={LINK} href={trust?.verify_docs} target="_blank" rel="noreferrer">
            Attestation guide
          </a>
          <a className={LINK} href={trust?.dstack_verifier} target="_blank" rel="noreferrer">
            dstack verifier
          </a>
        </span>
      </div>
      <div className="grid gap-3 @4xl/console:grid-cols-3">
        <TrustItem icon={<ShieldCheck />} title="Attestation">
          {attestation === undefined ? (
            <p className="text-muted-foreground">Loading…</p>
          ) : attestation.binding_verified ? (
            <p className="text-muted-foreground">
              <span className="font-medium text-success">Verified</span> for a fresh nonce: the TDX quote's report data
              binds this account's webhook key{" "}
              <code className="text-foreground" title={attestation.webhook_public_key}>
                {short(attestation.webhook_public_key ?? "")}
              </code>{" "}
              that signs every webhook ({attestation.quote_bytes ?? 0}-byte quote).
            </p>
          ) : (
            <p className="text-destructive">The attestation did not bind its keys.</p>
          )}
        </TrustItem>
        <TrustItem icon={<Cpu />} title="Application">
          {evidence == null ? (
            <p className="text-muted-foreground">TLS evidence unavailable.</p>
          ) : (
            <dl className="flex flex-col gap-2">
              <div>
                <dt className="text-muted-foreground">App id</dt>
                <dd className="font-mono break-all">{evidence.app_id}</dd>
              </div>
              {evidence.compose_hash !== undefined && (
                <div>
                  <dt className="text-muted-foreground">Compose hash</dt>
                  <dd className="font-mono" title={evidence.compose_hash}>
                    {short(evidence.compose_hash)}
                  </dd>
                </div>
              )}
            </dl>
          )}
          <p className="text-muted-foreground">From the TLS certificate evidence quote (at issuance).</p>
        </TrustItem>
        <TrustItem icon={<Wallet />} title="Non-custodial">
          <p className="text-muted-foreground">
            Every address pays only the merchant's treasury, fixed in the address. Phala Pay holds no funds and sends
            no transactions: the merchant sweeps and refunds itself.
          </p>
          <p className="text-muted-foreground">
            {networks === undefined ? "Networks: loading…" : `Networks: ${networks.map((each) => each.name).join(", ")}`}
          </p>
        </TrustItem>
      </div>
    </div>
  );
}

function TrustItem({ icon, title, children }: { icon: ReactNode; title: string; children: ReactNode }) {
  return (
    <section className="flex flex-col gap-2 rounded-lg bg-card p-4" aria-label={title}>
      <h4 className="flex items-center gap-2 text-sm font-medium [&_svg]:size-4 [&_svg]:text-muted-foreground">
        {icon}
        {title}
      </h4>
      {children}
    </section>
  );
}
