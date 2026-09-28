import { useEffect } from "react";
import { ApiError, type Account } from "./api.js";
import { short } from "./format.js";

export function ExplorerLink({
  account,
  kind,
  value,
}: {
  account: Account | null;
  kind: "address" | "tx";
  value: string;
}) {
  const explorer = account?.network.explorer ?? null;
  if (explorer === null) {
    return (
      <span className="mono" title={value}>
        {short(value)}
      </span>
    );
  }
  return (
    <a className="mono" href={`${explorer}/${kind}/${value}`} target="_blank" rel="noreferrer" title={value}>
      {short(value)}
    </a>
  );
}

export function describe(error: unknown): string {
  if (error instanceof ApiError) {
    return error.code === "rate_limited" ? "too many requests, try again in a minute" : error.code;
  }
  return "network error";
}

/** Calls `callback` now and every `interval` ms while `enabled`. */
export function usePolling(callback: () => void, interval: number, enabled = true): void {
  useEffect(() => {
    if (!enabled) {
      return;
    }
    callback();
    const timer = setInterval(callback, interval);
    return () => clearInterval(timer);
  }, [callback, interval, enabled]);
}

/** Triggers a download of `value` as a JSON file (a Blob URL: no request leaves the page). */
export function downloadJson(name: string, value: unknown): void {
  const url = URL.createObjectURL(new Blob([JSON.stringify(value, null, 2)], { type: "application/json" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.append(link);
  link.click();
  link.remove();
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
