import { formatUnits } from "viem";
import type { ClientQuote } from "./quote.js";

/**
 * The exact token amount to show, grouped for `locale` and without trailing zeros, for example
 * `1,273.9185`. Every digit is kept: the service rounds a quote to a few decimals.
 */
export function formatTokenAmount(quote: ClientQuote, locale?: string): string {
  const [whole = "0", fraction] = tokenAmount(quote).split(".");
  const format = new Intl.NumberFormat(locale);
  const grouped = format.format(BigInt(whole));
  if (fraction === undefined) {
    return grouped;
  }
  const separator = format.formatToParts(0.5).find((part) => part.type === "decimal")?.value ?? ".";
  return `${grouped}${separator}${fraction}`;
}

/** The exact token amount as a plain decimal, for copying into a wallet, for example `1273.9185`. */
export function tokenAmount(quote: ClientQuote): string {
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
