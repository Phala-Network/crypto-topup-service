import { formatUnits } from "viem";
import type { ClientQuote } from "./quote.js";

/** The exact token amount, for example `100.502512562814070352`. */
export function formatTokenAmount(quote: ClientQuote): string {
  return formatUnits(BigInt(quote.amount_atomic), quote.decimals);
}

/** The credit in the quote's currency, for example `$25.00`. */
export function formatAmount(quote: ClientQuote, locale?: string): string {
  return new Intl.NumberFormat(locale, {
    style: "currency",
    currency: quote.currency.toUpperCase(),
  }).format(quote.amount / 100);
}

/** `mm:ss` (or `h:mm:ss`) until `expiresAt` Unix seconds, never negative. */
export function formatCountdown(expiresAt: number, nowMs: number): string {
  const total = Math.max(0, Math.ceil(expiresAt - nowMs / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = String(total % 60).padStart(2, "0");
  return hours > 0
    ? `${hours}:${String(minutes).padStart(2, "0")}:${seconds}`
    : `${minutes}:${seconds}`;
}
