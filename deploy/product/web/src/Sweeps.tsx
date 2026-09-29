import { useMutation } from "@tanstack/react-query";
import { Button } from "@/components/ui/button";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import type { Account, FlushCall } from "./api.js";
import { Detail, Details, Disclosure, Empty, ExplorerLink, InfoTip, downloadJson, errorMessage, wallet } from "./common.js";
import { time, tokens } from "./format.js";
import { useSweeps } from "./queries.js";
import { Requests } from "./Timeline.js";

/**
 * Sweeping is the merchant's own transaction (design D4): Phala Pay never sweeps and holds no
 * key. The product's SDK builds `factory.flush(treasury, salts, token)` offline from the
 * forwarders the service lists as sweepable, keeping only those its pins derive, and a Safe
 * Transaction Builder batch of the same call for a Safe treasury. The flush is permissionless:
 * whoever sends it pays the gas, and the funds can only reach the treasury.
 */
export function Sweeps({ account }: { account: Account }) {
  const view = useSweeps().data ?? null;
  const send = useMutation({
    mutationFn: async (call: FlushCall) => {
      const { sendCall } = await wallet();
      return sendCall(account.network.chain_id, call);
    },
  });
  const symbol = account.token.symbol;
  const flush = view?.flush[0];
  return (
    <div className="@container flex flex-col gap-5 text-xs">
      <p className="flex items-center gap-1.5 text-muted-foreground">
        <span>
          Phala Pay never sweeps: the merchant signs <code>factory.flush(treasury, salts, token)</code> and pays the
          gas.
        </span>
        <InfoTip label="About sweeps">
          Payments stay in their forwarder addresses until the merchant sweeps them, from its own wallet or from its
          Safe through the Transaction Builder. Each forwarder can pay only the treasury fixed in its address, so
          anyone may send the call. The service marks deposits swept from the finalized Flushed events.
        </InfoTip>
      </p>
      {view === null ? (
        <p className="text-muted-foreground" aria-busy="true">
          Loading…
        </p>
      ) : (
        <div className="grid gap-6 @4xl:grid-cols-2 @4xl:gap-8">
          <div className="flex min-w-0 flex-col gap-4">
            <Details data-testid="unswept" className="tabular-nums">
              <Detail label="Unswept">{tokens(view.unswept_atomic, symbol)}</Detail>
              <Detail label="Final, sweepable">
                {tokens(view.final_unswept_atomic, symbol)} in {view.sweepable_forwarders} forwarder
                {view.sweepable_forwarders === 1 ? "" : "s"}
                {view.refused_forwarders > 0 && ` (${view.refused_forwarders} refused: not derivable from the pins)`}
              </Detail>
              <Detail label="Treasury">
                <ExplorerLink account={account} kind="address" value={view.treasury} /> (this demo's is Phala's
                finance Safe)
              </Detail>
            </Details>
            {flush === undefined ? (
              <p className="text-muted-foreground">Nothing to sweep: no final unswept balance.</p>
            ) : (
              <div className="flex flex-col gap-3">
                <Disclosure summary={`The flush the SDK built (${view.flush.length} call${view.flush.length === 1 ? "" : "s"})`}>
                  <pre className="max-h-60 overflow-auto rounded-lg bg-muted p-3 font-mono text-[0.6875rem] leading-relaxed">
                    {JSON.stringify(view.flush, null, 2)}
                  </pre>
                </Disclosure>
                <div className="flex flex-wrap gap-2">
                  <Button type="button" disabled={send.isPending} onClick={() => send.mutate(flush)}>
                    {send.isPending ? "Confirm in your wallet…" : "Sign the flush from my wallet"}
                  </Button>
                  <Button
                    type="button"
                    variant="outline"
                    onClick={() => downloadJson("phala-pay-sweep.json", view.safe_batch)}
                  >
                    Download Safe Transaction Builder batch
                  </Button>
                </div>
                <p className="text-muted-foreground wrap-anywhere" aria-live="polite" data-testid="flush-status">
                  {send.isSuccess && `Flush sent: ${send.data}. It is indexed once final.`}
                  {send.isError && errorMessage(send.error, "The wallet did not send it.")}
                </p>
              </div>
            )}
          </div>
          <div className="flex min-w-0 flex-col gap-3">
            <h3 className="text-[0.8125rem] font-medium">Finalized sweeps</h3>
            {view.sweeps.length === 0 ? (
              <Empty>None yet.</Empty>
            ) : (
              <Table className="text-xs">
                <TableHeader>
                  <TableRow>
                    <TableHead scope="col">Indexed</TableHead>
                    <TableHead scope="col">Forwarder</TableHead>
                    <TableHead scope="col">Amount</TableHead>
                    <TableHead scope="col">Flush transaction</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {view.sweeps.map((sweep) => (
                    <TableRow key={sweep.id} data-testid="sweep">
                      <TableCell>{time(sweep.created)}</TableCell>
                      <TableCell>
                        <ExplorerLink account={account} kind="address" value={sweep.address} />
                      </TableCell>
                      <TableCell>{tokens(sweep.amount_atomic, symbol)}</TableCell>
                      <TableCell>
                        <ExplorerLink account={account} kind="tx" value={sweep.tx_hash} />
                      </TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
            <Requests exchanges={view.api} title="API requests" id="sweeps-api-title" />
          </div>
        </div>
      )}
    </div>
  );
}
