// The product's demo API (deploy/product/reference_product/demo.py). The browser only talks to the
// product, which signs every service request itself, and to the service's public quote view,
// which the checkout reads with the quote's client secret.

export interface Account {
  account_id: string;
  balance: number;
  presets: number[];
  min_amount: number;
  max_amount: number;
  api_base: string;
  network: { chain_id: number; name: string; explorer: string | null; testnet: boolean };
  token: { symbol: string; address: string };
  transactions: Transaction[];
}

export interface Transaction {
  quote: string;
  created: number;
  amount: number;
  amount_atomic: string;
  status: string;
  deposit: string | null;
  tx_hash: string | null;
  paid_atomic: string | null;
  credited: number | null;
  refunded_atomic: string;
}

export type StepKey =
  | "quote_created"
  | "transfer_seen"
  | "finalized"
  | "credited"
  | "webhook_received"
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
}

export interface ApiExchange {
  method: string;
  url: string;
  status: number;
  request: { headers: Record<string, string>; body: unknown };
  response: unknown;
}

export interface Timeline {
  quote: { id: string; status: string; amount: number; amount_atomic: string; expires_at: number };
  steps: Step[];
  events: WebhookEvent[];
  api: ApiExchange[];
}

export interface Trust {
  attestation: {
    binding_verified: boolean;
    keyid?: string;
    settlement_pubkey?: string;
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

export async function getAccount(): Promise<Account> {
  const body = await request("account");
  return expect<Account>(body, ["account_id", "balance", "transactions", "network", "token"]);
}

export async function createQuote(amount: number): Promise<CreatedQuote> {
  const body = await request("quotes", {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ amount }),
  });
  return expect<CreatedQuote>(body, ["quote", "client_secret", "api"]);
}

export async function getTimeline(quote: string): Promise<Timeline> {
  const body = await request(`quotes/${encodeURIComponent(quote)}`);
  return expect<Timeline>(body, ["quote", "steps", "events", "api"]);
}

export async function getTrust(): Promise<Trust> {
  const body = await request("trust");
  return expect<Trust>(body, ["attestation", "verify_docs"]);
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
