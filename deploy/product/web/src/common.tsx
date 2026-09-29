import { ChevronRight, Info } from "lucide-react";
import type { ComponentProps, ReactNode } from "react";
import { Badge } from "@/components/ui/badge";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";
import { ApiError } from "./api.js";
import { networkOf } from "./chains.js";
import { short } from "./format.js";
import { useNetworks } from "./queries.js";

/** An inline text link, in the page's text colour. */
export const LINK = "font-medium underline decoration-foreground/30 underline-offset-4 transition-colors hover:decoration-foreground";

/**
 * The visitor's wallet helpers (./testTokens), loaded on first use: they carry the chain and wallet
 * libraries, which the page's first paint does not need.
 */
export function wallet() {
  return import("./testTokens.js");
}

/** The SDK's components, loaded when a payment starts: they carry the chain and wallet libraries. */
export function loadSdk() {
  return import("@phala/pay/react");
}

/** The primary action: Phala's lime, used for this and little else. */
export const BRAND_BUTTON =
  "h-11 w-full rounded-lg bg-brand text-[0.9375rem] font-semibold text-brand-foreground shadow-[inset_0_-1px_0_rgb(0_0_0/0.12)] hover:bg-brand/85 dark:shadow-none";

/** A transaction or address, linked to its chain's explorer, with the full value in a tooltip. */
export function ExplorerLink({
  chainId,
  kind,
  value,
}: {
  chainId: number | undefined;
  kind: "address" | "tx";
  value: string;
}) {
  const explorer = networkOf(useNetworks().data, chainId)?.explorer ?? null;
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
      <TooltipContent className="font-mono break-all">{value}</TooltipContent>
    </Tooltip>
  );
}

/** An explanation behind a small info icon: the page shows one short line, the tooltip the rest. */
export function InfoTip({ label, children, className }: { label: string; children: ReactNode; className?: string }) {
  return (
    <Tooltip>
      <TooltipTrigger asChild>
        <button
          type="button"
          aria-label={label}
          className={cn(
            "inline-flex size-4 shrink-0 translate-y-[0.1875rem] items-center justify-center rounded-full align-baseline text-muted-foreground transition-colors outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring",
            className,
          )}
        >
          <Info className="size-3.5" aria-hidden="true" />
        </button>
      </TooltipTrigger>
      <TooltipContent>{children}</TooltipContent>
    </Tooltip>
  );
}

/** A list of labelled values, such as a timeline step's details. */
export function Details({ className, ...props }: ComponentProps<"dl">) {
  return <dl className={cn("grid gap-1.5 rounded-lg bg-muted p-3 text-xs", className)} {...props} />;
}

export function Detail({ label, className, ...props }: ComponentProps<"dd"> & { label: ReactNode }) {
  return (
    <div className="grid grid-cols-[minmax(0,7rem)_minmax(0,1fr)] gap-3 sm:grid-cols-[minmax(0,9rem)_minmax(0,1fr)]">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className={cn("wrap-anywhere", className)} {...props} />
    </div>
  );
}

/** A titled part of a panel. */
export function Subsection({
  title,
  id,
  aside,
  children,
  className,
}: {
  title: string;
  id: string;
  aside?: ReactNode;
  children: ReactNode;
  className?: string;
}) {
  return (
    <section className={cn("flex min-w-0 flex-col gap-3 text-xs", className)} aria-labelledby={id}>
      <div className="flex items-center gap-2">
        <h3 id={id} className="text-[0.8125rem] font-medium">
          {title}
        </h3>
        {aside}
      </div>
      {children}
    </section>
  );
}

/** Secondary detail, collapsed until opened. */
export function Disclosure({ summary, children }: { summary: ReactNode; children: ReactNode }) {
  return (
    <details className="group/disclosure text-xs">
      <summary className="flex w-fit cursor-pointer list-none items-center gap-1 rounded-sm text-[0.8125rem] font-medium outline-none focus-visible:ring-2 focus-visible:ring-ring [&::-webkit-details-marker]:hidden">
        <ChevronRight
          className="size-3.5 shrink-0 text-muted-foreground transition-transform group-open/disclosure:rotate-90 motion-reduce:transition-none"
          aria-hidden="true"
        />
        {summary}
      </summary>
      <div className="mt-3 flex flex-col gap-2">{children}</div>
    </details>
  );
}

/** A panel's message while it has nothing to show. */
export function Empty({ children }: { children: ReactNode }) {
  return <p className="rounded-lg bg-muted px-4 py-5 text-xs text-muted-foreground">{children}</p>;
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

export function errorMessage(error: unknown, fallback: string): string {
  const message = error instanceof Error ? error.message.split("\n")[0] : undefined;
  return message ?? fallback;
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
