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

/** `12 s`, `3 min 5 s`, or `1 h 2 min`. */
export function duration(seconds: number): string {
  const s = Math.max(0, Math.round(seconds));
  if (s < 60) {
    return `${s} s`;
  }
  if (s < 3600) {
    return `${Math.floor(s / 60)} min ${s % 60} s`;
  }
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
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
