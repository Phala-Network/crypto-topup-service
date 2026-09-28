import { useEffect, type ComponentProps, type ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { ApiError, type Account } from "./api.js";
import { short } from "./format.js";

/** An inline text link, in the page's text colour. */
export const LINK = "font-medium underline underline-offset-4 hover:text-muted-foreground";

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
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        {explorer === null ? (
          <span className="font-mono text-xs">{short(value)}</span>
        ) : (
          <a className={cn("font-mono text-xs", LINK)} href={`${explorer}/${kind}/${value}`} target="_blank" rel="noreferrer">
            {short(value)}
          </a>
        )}
      </TooltipTrigger>
      <TooltipContent className="max-w-sm font-mono break-all">{value}</TooltipContent>
    </Tooltip>
  );
}

/** A list of labelled values, such as a timeline step's details. */
export function Details({ className, ...props }: ComponentProps<"dl">) {
  return <dl className={cn("grid gap-1.5 rounded-lg bg-muted/60 p-3 text-xs", className)} {...props} />;
}

export function Detail({ label, className, ...props }: ComponentProps<"dd"> & { label: ReactNode }) {
  return (
    <div className="grid grid-cols-[minmax(0,7rem)_minmax(0,1fr)] gap-3 sm:grid-cols-[minmax(0,9rem)_minmax(0,1fr)]">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={cn("wrap-anywhere", className)} {...props} />
    </div>
  );
}

/** A part of a card below a divider; next to the timeline, the first one needs none. */
export function Subsection({
  title,
  id,
  children,
}: {
  title: string;
  id: string;
  children: ReactNode;
}) {
  return (
    <section
      className="flex flex-col gap-2 border-t pt-4 text-xs @4xl:first:border-t-0 @4xl:first:pt-0"
      aria-labelledby={id}
    >
      <h3 id={id} className="text-sm font-medium">
        {title}
      </h3>
      {children}
    </section>
  );
}

const TONES: Record<string, "success" | "danger"> = {
  credited: "success",
  succeeded: "success",
  swept: "success",
  rejected: "danger",
  expired: "danger",
  reversed: "danger",
  failed: "danger",
};

/** A payment's or refund's status: success and failure in their colours, anything else neutral. */
export function StatusBadge({ status, children }: { status: string; children: ReactNode }) {
  const tone = TONES[status];
  return (
    <Badge
      variant={tone === "danger" ? "destructive" : "secondary"}
      className={tone === "success" ? "bg-success/15 text-success" : undefined}
    >
      {children}
    </Badge>
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
