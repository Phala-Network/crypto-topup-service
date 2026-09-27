import { parseClientQuote, quoteIdFromClientSecret, type ClientQuote } from "./quote.js";

/**
 * What the payer sees:
 * - `loading` until the quote is first read;
 * - `waiting` for a payment, until `expires_at`;
 * - `seen` once a transfer is in a block, `confirming` once it is final and being valued;
 * - `credited`, or `rejected` for a final payment that will not be credited;
 * - `expired` or `canceled` without a payment;
 * - `error` when the client secret is not valid.
 */
export type CheckoutStatus =
  | "loading"
  | "waiting"
  | "seen"
  | "confirming"
  | "credited"
  | "rejected"
  | "expired"
  | "canceled"
  | "error";

export type CheckoutErrorCode =
  | "invalid_client_secret"
  | "rate_limited"
  | "network_error"
  | "invalid_response"
  | "api_error";

export class CheckoutError extends Error {
  override readonly name = "CheckoutError";

  constructor(
    readonly code: CheckoutErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}

export interface CheckoutState {
  status: CheckoutStatus;
  quote: ClientQuote | null;
  /** The last failed refresh; polling continues after every error but `invalid_client_secret`. */
  error: CheckoutError | null;
}

export interface CheckoutOptions {
  /** The quote's `client_secret`, from your backend's `POST /v1/quotes`. */
  clientSecret: string;
  /** The service origin, for example `https://topup.example.com`. */
  apiBase: string;
  /** Milliseconds between status reads; default 3000. */
  pollInterval?: number;
  fetch?: typeof globalThis.fetch;
  /** Current time in milliseconds; for tests. */
  now?: () => number;
}

export interface CheckoutSession {
  getState(): CheckoutState;
  /** Calls `listener` on every state change; returns the unsubscribe function. */
  subscribe(listener: (state: CheckoutState) => void): () => void;
  /** Reads the quote now instead of at the next poll. */
  refresh(): Promise<void>;
  /** Stops polling. */
  destroy(): void;
}

const DEFAULT_POLL_INTERVAL = 3000;
const MAX_BACKOFF = 30_000;

export interface RetrieveQuoteOptions {
  clientSecret: string;
  apiBase: string;
  fetch?: typeof globalThis.fetch;
}

/**
 * Reads a quote's public view once, as Stripe.js's `retrievePaymentIntent(clientSecret)` does.
 * Rejects with a `CheckoutError`: `invalid_client_secret` for an unknown quote or secret.
 */
export async function retrieveQuote(options: RetrieveQuoteOptions): Promise<ClientQuote> {
  const quoteId = quoteIdFromClientSecret(options.clientSecret);
  const base = options.apiBase.replace(/\/+$/, "");
  const url = `${base}/v1/quotes/${quoteId}?client_secret=${encodeURIComponent(options.clientSecret)}`;
  const fetchImpl = options.fetch ?? globalThis.fetch.bind(globalThis);
  // A simple GET with no custom headers, so the browser sends no CORS preflight.
  const response = await fetchImpl(url, { cache: "no-store", credentials: "omit" });
  if (response.status === 404) {
    throw new CheckoutError("invalid_client_secret", "the quote or its client secret is unknown");
  }
  if (response.status === 429) {
    throw new CheckoutError("rate_limited", "too many status requests");
  }
  if (!response.ok) {
    throw new CheckoutError("api_error", `the payment service answered ${response.status}`);
  }
  try {
    return parseClientQuote(await response.json());
  } catch (cause) {
    throw new CheckoutError("invalid_response", "unexpected response from the payment service", {
      cause,
    });
  }
}

/** The payer-facing status of a quote at `nowSeconds`. */
export function checkoutStatus(quote: ClientQuote, nowSeconds: number): CheckoutStatus {
  if (quote.payment_status !== "none") {
    return quote.payment_status;
  }
  if (quote.status === "canceled") {
    return "canceled";
  }
  if (quote.status === "expired" || nowSeconds >= quote.expires_at) {
    return "expired";
  }
  return quote.status === "open" ? "waiting" : "confirming";
}

/**
 * Follows a quote from the browser by polling its public view until it is credited, rejected,
 * canceled, or expired by the service. The status turns `expired` at `expires_at` even while the
 * service, which expires quotes by chain time, still reports the quote open; polling continues
 * until then, so a payment sent just before expiry still shows.
 */
export function createCheckout(options: CheckoutOptions): CheckoutSession {
  quoteIdFromClientSecret(options.clientSecret);
  const now = options.now ?? Date.now;
  const interval = options.pollInterval ?? DEFAULT_POLL_INTERVAL;
  const listeners = new Set<(state: CheckoutState) => void>();

  let state: CheckoutState = { status: "loading", quote: null, error: null };
  let timer: ReturnType<typeof setTimeout> | undefined;
  let failures = 0;
  let destroyed = false;
  let inFlight: Promise<void> | undefined;

  function setState(next: CheckoutState): void {
    if (
      next.status === state.status &&
      next.error === state.error &&
      JSON.stringify(next.quote) === JSON.stringify(state.quote)
    ) {
      return;
    }
    state = next;
    for (const listener of listeners) {
      listener(state);
    }
  }

  function finished(): boolean {
    const { status, quote } = state;
    return (
      destroyed ||
      status === "error" ||
      status === "credited" ||
      status === "rejected" ||
      status === "canceled" ||
      (status === "expired" && quote?.status === "expired")
    );
  }

  async function load(): Promise<void> {
    try {
      const quote = await retrieveQuote(options);
      failures = 0;
      setState({ status: checkoutStatus(quote, now() / 1000), quote, error: null });
    } catch (cause) {
      const error =
        cause instanceof CheckoutError
          ? cause
          : new CheckoutError("network_error", "could not reach the payment service", { cause });
      failures += 1;
      const quote = state.quote;
      if (error.code === "invalid_client_secret") {
        setState({ status: "error", quote, error });
      } else {
        setState({
          status: quote === null ? state.status : checkoutStatus(quote, now() / 1000),
          quote,
          error,
        });
      }
    }
  }

  function refresh(): Promise<void> {
    inFlight ??= load().finally(() => {
      inFlight = undefined;
    });
    return inFlight;
  }

  function schedule(): void {
    if (finished()) {
      return;
    }
    const delay = failures === 0 ? interval : Math.min(interval * 2 ** failures, MAX_BACKOFF);
    timer = setTimeout(() => {
      void refresh().then(schedule);
    }, delay);
  }

  void refresh().then(schedule);

  return {
    getState: () => state,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    refresh,
    destroy() {
      destroyed = true;
      clearTimeout(timer);
      listeners.clear();
    },
  };
}
