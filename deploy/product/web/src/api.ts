// The product's demo API (deploy/product/reference_product/demo.py). The browser only talks to the
// product, which sends every service request itself with its API key, and to the service's public
// quote and deposit address views, which the SDK components read with a client secret.

import type { DepositAddressDetails } from "@phala/pay";

export interface Account {
  account_id: string;
  /** Cents: the console's ledger, credits less their refunded and reversed shares. */
  balance: number;
  ledger: LedgerLine[];
  presets: number[];
  min_amount: number;
  max_amount: number;
  api_base: string;
  network: { chain_id: number; name: string; explorer: string | null; testnet: boolean };
  token: { symbol: string; address: string };
  treasury: string;
  factory: string;
  deposit_address: string | null;
  payments: PaymentRow[];
}

export interface LedgerLine {
  deposit: string;
  amount: number;
  /** `deposit.credited`, or the event that adjusted the credit (`deposit.refunded`, …). */
  reason: string;
  at: number;
}

export interface PaymentRow {
  kind: "quote" | "address";
  /** The deposit's `dep_` id, or the quote's `qt_` id while it has no deposit. */
  id: string;
  quote: string | null;
  created: number;
  amount: number | null;
  amount_atomic: string;
  status: string;
  final: boolean;
  swept: boolean;
  tx_hash: string | null;
  amount_refunded_atomic: string;
  net: number | null;
}

export type StepKey =
  | "quote_created"
  | "sent"
  | "received"
  | "credited"
  | "webhook_received"
  | "final"
  | "reversed"
  | "swept";

export interface Detail {
  label: string;
  value: string | number | null;
  kind?: "address" | "tx" | "time" | "usd" | "usd_delta" | "atomic";
  mono?: boolean;
}

export interface Step {
  key: StepKey;
  state: "complete" | "current" | "upcoming" | "failed";
  at: number | null;
  details: Detail[];
}

export interface WebhookEvent {
  id: string;
  type: string;
  received_at: number;
  verified: boolean;
  data: { object?: Record<string, unknown> };
}

export interface ApiExchange {
  method: string;
  url: string;
  status: number;
  request: { headers: Record<string, string>; body: unknown };
  response: unknown;
}

export interface Deposit {
  id: string;
  status: string;
  final: boolean;
  /** When the service's finality watch found it final, Unix seconds; null until final. */
  final_at?: number | null;
  swept: boolean;
  amount: number | null;
  amount_atomic: string;
  amount_refunded_atomic: string;
  amount_refunded: number;
  amount_reversed: number;
  from_address: string;
  asset_contract: string;
  tx_hash: string;
  metadata: Record<string, string>;
}

export interface Refund {
  id: string;
  status: "pending" | "succeeded" | "failed" | "canceled";
  amount_atomic: string;
  destination_address: string;
  treasury: string;
  transaction_hash: string | null;
  receipt_log_index: number | null;
  failure_reason: string | null;
  failure_explanation: string | null;
  created: number;
  /** The exact transfer that pays the refund, while it awaits one. */
  transfer: { from: string; token: string; to: string; amount_atomic: string; data: string } | null;
}

export interface LedgerView {
  status: string;
  amount: number | null;
  amount_refunded: number;
  amount_reversed: number;
  /** What the snapshot rule nets the deposit to, from the service's deposit. */
  nets_to: number;
  product: {
    status: string | null;
    reason: string | null;
    credit: number | null;
    net: number | null;
    adjustments: { amount: number; reason: string; at: number }[];
  } | null;
}

export interface Timeline {
  kind: "quote" | "address";
  quote: { id: string; status: string; metadata: Record<string, string> } | null;
  deposit: Deposit | null;
  sent: { tx_hash: string; block_number: number; at: number } | null;
  steps: Step[];
  refunds: Refund[];
  ledger: LedgerView | null;
  events: WebhookEvent[];
  api: ApiExchange[];
}

export interface Trust {
  attestation: {
    binding_verified: boolean;
    account?: string;
    livemode?: boolean;
    webhook_public_key?: string;
    report_data?: string;
    quote_bytes?: number;
  };
  tls_evidence: { app_id: string; compose_hash?: string; os_image_hash?: string; url: string } | null;
  verify_docs: string;
  dstack_verifier: string;
}

export interface CreatedQuote {
  quote: string;
  client_secret: string;
  /** The quote's address as the product's SDK recomputed it from the pins. */
  expected_address: string;
  order_id: string;
  api: ApiExchange[];
}

export interface AddressPayment {
  status: string;
  tx_hash: string;
  amount_atomic: string;
  confirmations: number | null;
  /** The deposit's `dep_` id, known before it is recorded. */
  deposit: string;
}

export interface DepositAddressView extends DepositAddressDetails {
  id: string;
  version: number;
  status: string;
  metadata: Record<string, string>;
  networks: (DepositAddressDetails["networks"][number] & { treasury: string })[];
  payments: AddressPayment[];
}

export interface DepositAddressResponse {
  deposit_address: DepositAddressView;
  /** Only from `POST api/deposit_address`, for `<DepositAddress>`. */
  client_secret?: string;
  /** The product's SDK recomputed every network's address from the pins. */
  verified: boolean;
  api: ApiExchange[];
}

export interface FlushCall {
  to: string;
  data: string;
  value: string;
}

export interface Sweeps {
  chain_id: number;
  token: string;
  treasury: string;
  factory: string;
  unswept_atomic: string;
  final_unswept_atomic: string;
  sweepable_forwarders: number;
  refused_forwarders: number;
  flush: FlushCall[];
  safe_batch: unknown;
  sweeps: { id: string; address: string; amount_atomic: string; tx_hash: string; created: number }[];
  api: ApiExchange[];
}

export class ApiError extends Error {
  constructor(
    readonly status: number,
    readonly code: string,
  ) {
    super(code);
  }
}

// Relative to the page, which the product serves at `{public_url}/demo/`.
async function request(path: string, init?: RequestInit): Promise<unknown> {
  const response = await fetch(`api/${path}`, { credentials: "same-origin", ...init });
  const body: unknown = await response.json().catch(() => null);
  if (!response.ok) {
    const code = isRecord(body) && typeof body["code"] === "string" ? body["code"] : "error";
    throw new ApiError(response.status, code);
  }
  if (!isRecord(body)) {
    throw new ApiError(response.status, "invalid_response");
  }
  return body;
}

function post(path: string, body: unknown): Promise<unknown> {
  return request(path, {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  });
}

export async function getAccount(): Promise<Account> {
  const body = await request("account");
  return expect<Account>(body, ["account_id", "balance", "ledger", "payments", "network", "token"]);
}

export async function createQuote(amount: number): Promise<CreatedQuote> {
  const body = await post("quotes", { amount });
  return expect<CreatedQuote>(body, ["quote", "client_secret", "expected_address", "api"]);
}

export async function createDepositAddress(): Promise<DepositAddressResponse> {
  const body = await post("deposit_address", {});
  return expect<DepositAddressResponse>(body, ["deposit_address", "client_secret", "verified"]);
}

export async function getDepositAddress(): Promise<DepositAddressResponse> {
  const body = await request("deposit_address");
  return expect<DepositAddressResponse>(body, ["deposit_address", "verified"]);
}

export async function getTimeline(selection: Selection): Promise<Timeline> {
  const path = selection.kind === "quote" ? "quotes" : "deposits";
  const body = await request(`${path}/${encodeURIComponent(selection.id)}`);
  return expect<Timeline>(body, ["steps", "refunds", "events", "api"]);
}

export async function createRefund(
  deposit: string,
  amountAtomic: string,
  destinationAddress: string,
): Promise<Refund> {
  const body = await post("refunds", {
    deposit,
    amount_atomic: amountAtomic,
    destination_address: destinationAddress,
  });
  return expect<{ refund: Refund }>(body, ["refund"]).refund;
}

export async function markRefundPaid(
  refund: string,
  transactionHash: string,
  receiptLogIndex: number | null,
): Promise<Refund> {
  const body = await post(`refunds/${encodeURIComponent(refund)}/mark_paid`, {
    transaction_hash: transactionHash,
    ...(receiptLogIndex === null ? {} : { receipt_log_index: receiptLogIndex }),
  });
  return expect<{ refund: Refund }>(body, ["refund"]).refund;
}

export async function cancelRefund(refund: string): Promise<Refund> {
  const body = await post(`refunds/${encodeURIComponent(refund)}/cancel`, {});
  return expect<{ refund: Refund }>(body, ["refund"]).refund;
}

export async function getSweeps(): Promise<Sweeps> {
  const body = await request("sweeps");
  return expect<Sweeps>(body, ["unswept_atomic", "flush", "sweeps"]);
}

export async function getTrust(): Promise<Trust> {
  const body = await request("trust");
  return expect<Trust>(body, ["attestation", "verify_docs"]);
}

/** What the behind-the-scenes panel follows: a quote (before and after its payment) or a deposit. */
export interface Selection {
  kind: "quote" | "deposit";
  id: string;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

// The product is this page's own backend; a shape check on the fields read catches a mismatched
// deployment without duplicating every field's validation.
// eslint-disable-next-line @typescript-eslint/no-unnecessary-type-parameters -- the caller names the checked shape
function expect<T>(body: unknown, keys: string[]): T {
  if (!isRecord(body) || !keys.every((key) => key in body)) {
    throw new ApiError(200, "invalid_response");
  }
  return body as T;
}
