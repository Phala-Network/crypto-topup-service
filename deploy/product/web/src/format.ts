import { formatUnits } from "viem";

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function dollars(cents: number): string {
  return usd.format(cents / 100);
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

export function statusLabel(status: string): string {
  const labels: Record<string, string> = {
    awaiting_payment: "Awaiting payment",
    expired: "Expired",
    detected: "Detected",
    confirmed: "Confirming",
    credited: "Credited",
    swept: "Credited · swept",
    rejected: "Rejected",
  };
  return labels[status] ?? status;
}
