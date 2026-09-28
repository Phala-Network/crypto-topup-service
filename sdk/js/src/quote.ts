/**
 * The public view of a quote, as `GET /v1/quotes/{id}?client_secret=…` returns it to a browser.
 */
export interface ClientQuote {
  id: string;
  object: "quote";
  /** `false` for a test-mode quote, paid on a testnet; the page says so. */
  livemode: boolean;
  /** `open` until a matching payment completes it, it expires by chain time, or it is canceled. */
  status: "open" | "complete" | "expired" | "canceled";
  /** Credit in the currency's minor unit (US cents). */
  amount: number;
  currency: string;
  /** The token's code, for example `pha`. */
  asset: string;
  decimals: number;
  chain_id: number;
  /** The exact token amount to send, in the token's smallest unit, as a decimal string. */
  amount_atomic: string;
  /** The quote's single-use deposit address. */
  address: string;
  /** The EIP-681 payment request: token transfer of `amount_atomic` to `address`. */
  payment_uri: string;
  /** Unix seconds; after this the address must no longer be shown. */
  expires_at: number;
  /**
   * `seen` once in a block (a reorg can remove it), `confirming` while a payment at the route's
   * confirmation is valued and screened, `credited` once it is credited (about 30 seconds after
   * paying), `rejected` when it will not be, `reversed` when a credited payment's transaction
   * left the chain before finality (the payment did not happen).
   */
  payment_status: "none" | "seen" | "confirming" | "credited" | "rejected" | "reversed";
  /** Block confirmations while `payment_status` is `seen`, otherwise `null`. */
  confirmations: number | null;
}

const QUOTE_STATUSES = ["open", "complete", "expired", "canceled"] as const;
const PAYMENT_STATUSES = ["none", "seen", "confirming", "credited", "rejected", "reversed"] as const;

/** Returns the quote id a client secret belongs to (`qt_…_secret_…`), or throws. */
export function quoteIdFromClientSecret(clientSecret: string): string {
  const match = /^(qt_[0-9a-f]+)_secret_[0-9a-f]+$/.exec(clientSecret);
  if (match?.[1] === undefined) {
    throw new TypeError("clientSecret is not a quote client secret");
  }
  return match[1];
}

/** Validates a response body against the public quote view; throws on anything else. */
export function parseClientQuote(value: unknown): ClientQuote {
  if (typeof value !== "object" || value === null) {
    throw new TypeError("quote response is not an object");
  }
  const v = value as Record<string, unknown>;
  const confirmations = v["confirmations"];
  if (
    typeof v["id"] !== "string" ||
    v["object"] !== "quote" ||
    typeof v["livemode"] !== "boolean" ||
    !oneOf(v["status"], QUOTE_STATUSES) ||
    !isSafeInteger(v["amount"]) ||
    typeof v["currency"] !== "string" ||
    typeof v["asset"] !== "string" ||
    !isSafeInteger(v["decimals"]) ||
    !isSafeInteger(v["chain_id"]) ||
    typeof v["amount_atomic"] !== "string" ||
    !/^\d+$/.test(v["amount_atomic"]) ||
    typeof v["address"] !== "string" ||
    !/^0x[0-9a-fA-F]{40}$/.test(v["address"]) ||
    typeof v["payment_uri"] !== "string" ||
    !isSafeInteger(v["expires_at"]) ||
    !oneOf(v["payment_status"], PAYMENT_STATUSES) ||
    !(confirmations === null || confirmations === undefined || isSafeInteger(confirmations))
  ) {
    throw new TypeError("quote response does not match the public quote view");
  }
  return {
    id: v["id"],
    object: "quote",
    livemode: v["livemode"],
    status: v["status"],
    amount: v["amount"],
    currency: v["currency"],
    asset: v["asset"],
    decimals: v["decimals"],
    chain_id: v["chain_id"],
    amount_atomic: v["amount_atomic"],
    address: v["address"],
    payment_uri: v["payment_uri"],
    expires_at: v["expires_at"],
    payment_status: v["payment_status"],
    confirmations: confirmations ?? null,
  };
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[]): value is T {
  return typeof value === "string" && (allowed as readonly string[]).includes(value);
}

function isSafeInteger(value: unknown): value is number {
  return Number.isSafeInteger(value);
}
