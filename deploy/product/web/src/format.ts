import { formatUnits } from "viem";

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function dollars(cents: number): string {
  return usd.format(cents / 100);
}

/** `+$20.00` or `−$2.50`. */
export function signedDollars(cents: number): string {
  return `${cents < 0 ? "−" : "+"}${usd.format(Math.abs(cents) / 100)}`;
}

/** The exact token amount, grouped and without trailing zeros: `1,273.9185 PHA`. */
export function tokens(atomic: string, symbol: string, decimals = 18): string {
  const [whole = "0", fraction] = formatUnits(BigInt(atomic), decimals).split(".");
  const grouped = BigInt(whole).toLocaleString("en-US");
  return `${fraction === undefined ? grouped : `${grouped}.${fraction}`} ${symbol}`;
}

export function short(value: string): string {
  return value.length > 18 ? `${value.slice(0, 10)}…${value.slice(-6)}` : value;
}

export function time(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString("en-US", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** A table's date, `Sep 28, 15:21`. */
export function day(seconds: number): string {
  return new Date(seconds * 1000).toLocaleString("en-US", {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    hourCycle: "h23",
  });
}

/** The time of day, `15:21:05`: a log's timestamp. */
export function clock(seconds: number): string {
  return new Date(seconds * 1000).toLocaleTimeString("en-GB", {
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
  });
}

/** `12s`, `3m 5s`, or `1h 2m`. */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) {
    return `${s}s`;
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)}m ${s % 60}s`;
  }
  return `${Math.floor(s / 3600)}h ${Math.floor((s % 3600) / 60)}m`;
}

export function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    awaiting_payment: "Awaiting payment",
    expired: "Expired",
    pending: "Pending",
    credited: "Credited",
    rejected: "Rejected",
    reversed: "Reversed",
    succeeded: "Succeeded",
    failed: "Failed",
    canceled: "Canceled",
  };
  return labels[status] ?? status;
}

// A rate to 4 significant digits, but never fewer than whole cents: `$0.06041`, `$0.25`, `$1.00`,
// `$1,234.57`.
const precise = new Intl.NumberFormat("en-US", {
  style: "currency",
  currency: "USD",
  maximumFractionDigits: 2,
  maximumSignificantDigits: 4,
  roundingPriority: "morePrecision",
});

/** A USD-per-token rate, as the service states it (8 decimals), to 4 significant digits. */
export function price(exchangeRate: string): string {
  const value = Number(exchangeRate);
  const fraction = precise.formatToParts(value).find((part) => part.type === "fraction")?.value.length ?? 0;
  return fraction < 2 ? usd.format(value) : precise.format(value);
}

/** `1 PHA = $0.06041`. */
export function rate(symbol: string, exchangeRate: string): string {
  return `1 ${symbol} = ${price(exchangeRate)}`;
}

/** Basis points as a percentage: `10%`, `2.5%`. */
export function percent(bps: number): string {
  return `${new Intl.NumberFormat("en-US", { maximumFractionDigits: 2 }).format(bps / 100)}%`;
}

/** A token as the customer sees it: `Test PHA` on a testnet, so it is never taken for real money. */
export function tokenName(symbol: string, testnet: boolean): string {
  return testnet ? `Test ${symbol}` : symbol;
}
