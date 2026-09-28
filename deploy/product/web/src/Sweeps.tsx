import { useCallback, useState } from "react";
import { getSweeps, type Account, type Sweeps as SweepsView } from "./api.js";
import { ExplorerLink, downloadJson, usePolling } from "./common.js";
import { time, tokens } from "./format.js";
import { DeveloperView } from "./Timeline.js";
import { errorMessage, sendCall } from "./testTokens.js";

/**
 * Sweeping is the merchant's own transaction (design D4): Phala Pay never sweeps and holds no
 * key. The product's SDK builds `factory.flush(treasury, salts, token)` offline from the
 * forwarders the service lists as sweepable, keeping only those its pins derive, and a Safe
 * Transaction Builder batch of the same call for a Safe treasury. The flush is permissionless:
 * whoever sends it pays the gas, and the funds can only reach the treasury.
 */
export function Sweeps({ account }: { account: Account }) {
  const [view, setView] = useState<SweepsView | null>(null);
  const [state, setState] = useState<{ pending: boolean; text: string | null }>({ pending: false, text: null });
  const refresh = useCallback(() => {
    getSweeps().then(setView, () => undefined);
  }, []);
  usePolling(refresh, 10_000);
  const symbol = account.token.symbol;
  const flush = view?.flush[0];
  return (
    <section className="card" aria-labelledby="sweeps-title">
      <h2 id="sweeps-title">Sweeps: the merchant's transaction</h2>
      <p className="muted small">
        Payments stay in their forwarder addresses until the merchant sweeps them. Phala Pay never
        sweeps: the merchant signs <code>factory.flush(treasury, salts, token)</code> from its own
        wallet, or from its Safe through the Transaction Builder, and pays the gas. Each forwarder
        can pay only the treasury fixed in its address, so anyone may send the call. The service
        marks deposits swept from the finalized <code>Flushed</code> events.
      </p>
      {view === null ? (
        <p className="muted small">Loading…</p>
      ) : (
        <>
          <dl className="details" data-testid="unswept">
            <div>
              <dt>Unswept</dt>
              <dd>{tokens(view.unswept_atomic, symbol)}</dd>
            </div>
            <div>
              <dt>Final, sweepable</dt>
              <dd>
                {tokens(view.final_unswept_atomic, symbol)} in {view.sweepable_forwarders} forwarder
                {view.sweepable_forwarders === 1 ? "" : "s"}
                {view.refused_forwarders > 0 && ` (${view.refused_forwarders} refused: not derivable from the pins)`}
              </dd>
            </div>
            <div>
              <dt>Treasury</dt>
              <dd>
                <ExplorerLink account={account} kind="address" value={view.treasury} /> (this demo's is Phala's
                finance Safe)
              </dd>
            </div>
          </dl>
          {flush === undefined ? (
            <p className="small">Nothing to sweep: no final unswept balance.</p>
          ) : (
            <div className="stack">
              <details>
                <summary>The flush the SDK built ({view.flush.length} call{view.flush.length === 1 ? "" : "s"})</summary>
                <pre className="code">{JSON.stringify(view.flush, null, 2)}</pre>
              </details>
              <div className="actions">
                <button
                  type="button"
                  className="secondary"
                  disabled={state.pending}
                  onClick={() => {
                    setState({ pending: true, text: null });
                    sendCall(account.network.chain_id, flush).then(
                      (hash) => {
                        setState({ pending: false, text: `Flush sent: ${hash}. It is indexed once final.` });
                      },
                      (error: unknown) =>
                        setState({ pending: false, text: errorMessage(error, "The wallet did not send it.") }),
                    );
                  }}
                >
                  {state.pending ? "Confirm in your wallet…" : "Sign the flush from my wallet"}
                </button>
                <button
                  type="button"
                  className="ghost"
                  onClick={() => downloadJson("phala-pay-sweep.json", view.safe_batch)}
                >
                  Download Safe Transaction Builder batch
                </button>
              </div>
              <p className="muted small" aria-live="polite" data-testid="flush-status">
                {state.text}
              </p>
            </div>
          )}
          <h3>Finalized sweeps</h3>
          {view.sweeps.length === 0 ? (
            <p className="muted small">None yet.</p>
          ) : (
            <div className="table-scroll">
              <table className="table compact">
                <thead>
                  <tr>
                    <th scope="col">Indexed</th>
                    <th scope="col">Forwarder</th>
                    <th scope="col">Amount</th>
                    <th scope="col">Flush transaction</th>
                  </tr>
                </thead>
                <tbody>
                  {view.sweeps.map((sweep) => (
                    <tr key={sweep.id} data-testid="sweep">
                      <td>{time(sweep.created)}</td>
                      <td>
                        <ExplorerLink account={account} kind="address" value={sweep.address} />
                      </td>
                      <td>{tokens(sweep.amount_atomic, symbol)}</td>
                      <td>
                        <ExplorerLink account={account} kind="tx" value={sweep.tx_hash} />
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
          <DeveloperView exchanges={view.api} title="Developer view: GET /v1/balance, /v1/forwarders, /v1/sweeps" />
        </>
      )}
    </section>
  );
}
