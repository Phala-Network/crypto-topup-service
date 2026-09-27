import { formatUnits } from "viem";

const usd = new Intl.NumberFormat("en-US", { style: "currency", currency: "USD" });

export function dollars(cents: number): string {
  return usd.format(cents / 100);
}

export function tokens(atomic: string, symbol: string, decimals = 18): string {
  const value = formatUnits(BigInt(atomic), decimals);
  const [whole = "0", fraction = ""] = value.split(".");
  const trimmed = fraction.slice(0, 6).replace(/0+$/, "");
  return `${Number(whole).toLocaleString("en-US")}${trimmed ? `.${trimmed}` : ""} ${symbol}`;
}

export function exactTokens(atomic: string, symbol: string, decimals = 18): string {
  return `${formatUnits(BigInt(atomic), decimals)} ${symbol}`;
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
