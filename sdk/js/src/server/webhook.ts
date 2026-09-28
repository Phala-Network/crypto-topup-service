/**
 * Standard Webhooks `v1a` verification of Phala Pay deliveries (design D11): an ed25519 signature
 * over `{webhook-id}.{webhook-timestamp}.{body}` by your account's webhook key in the endpoint's
 * mode, pinned from `GET /v1/attestation`. Uses WebCrypto, so it runs on Node 20+, Deno, Bun, and
 * edge runtimes.
 */

/** A delivery that did not verify: answer `400` and do nothing. */
export class WebhookSignatureError extends Error {
  override readonly name = "WebhookSignatureError";
}

/** A verified event. `data.object` is the object the event is about, such as a deposit. */
export interface WebhookEvent {
  /** `evt_…`, stable across retries and resends: process each id once. */
  id: string;
  object: "event";
  /** Your account, `acct_…`. */
  account: string;
  livemode: boolean;
  /** Such as `deposit.credited`; claw back a `deposit.reversed` deposit's credit. */
  type: string;
  created: number;
  actor?: string;
  data: { object: Record<string, unknown>; previous_attributes?: Record<string, unknown> };
}

export interface ConstructEventOptions {
  /** Your account id, `acct_…`: an event of another account fails closed. */
  expectedAccount: string;
  /** The mode of the endpoint receiving it: an event of the other mode fails closed. */
  expectedLivemode: boolean;
  /** Seconds a delivery's timestamp may differ from now; default 300. */
  tolerance?: number;
  /** Current time in Unix seconds; for tests. */
  now?: number;
}

/**
 * Verifies a delivery and returns its event, as Stripe's `constructEvent` does, failing closed
 * unless a `v1a` signature verifies with one of `publicKeys` (hex or base64 raw ed25519 keys; pass
 * both while a rotation overlaps), the timestamp is within tolerance, the body's id is the
 * `webhook-id`, and the event is `expectedAccount`'s in `expectedLivemode`.
 *
 * `payload` is the raw request body, before any JSON parsing.
 */
export async function constructEvent(
  payload: string | Uint8Array,
  headers: Headers | Record<string, string | string[] | undefined>,
  publicKeys: string | readonly string[],
  options: ConstructEventOptions,
): Promise<WebhookEvent> {
  if (options.expectedAccount === "") {
    throw new TypeError("expectedAccount is required");
  }
  const body = typeof payload === "string" ? new TextEncoder().encode(payload) : payload;
  const id = header(headers, "webhook-id");
  const timestamp = header(headers, "webhook-timestamp");
  const signatures = header(headers, "webhook-signature");
  if (id === undefined || timestamp === undefined || signatures === undefined) {
    throw new WebhookSignatureError("webhook headers missing");
  }
  if (!/^\d+$/.test(timestamp)) {
    throw new WebhookSignatureError("webhook timestamp malformed");
  }
  const now = options.now ?? Math.floor(Date.now() / 1000);
  if (Math.abs(now - Number(timestamp)) > (options.tolerance ?? 300)) {
    throw new WebhookSignatureError("webhook timestamp outside tolerance");
  }
  const signed = concat(new TextEncoder().encode(`${id}.${timestamp}.`), body);
  const keys = await Promise.all(
    (typeof publicKeys === "string" ? [publicKeys] : publicKeys).map(importKey),
  );
  if (keys.length === 0) {
    throw new TypeError("no webhook public key pinned");
  }
  let verified = false;
  for (const entry of signatures.split(" ")) {
    const [version, encoded] = entry.split(",", 2);
    const signature = version === "v1a" && encoded !== undefined ? base64(encoded) : undefined;
    if (signature === undefined) {
      continue;
    }
    for (const key of keys) {
      if (await crypto.subtle.verify("Ed25519", key, signature, signed)) {
        verified = true;
      }
    }
  }
  if (!verified) {
    throw new WebhookSignatureError("no valid webhook signature");
  }
  let event: unknown;
  try {
    event = JSON.parse(new TextDecoder().decode(body));
  } catch {
    throw new TypeError("webhook body is not an event");
  }
  if (typeof event !== "object" || event === null) {
    throw new TypeError("webhook body is not an event");
  }
  const e = event as Record<string, unknown>;
  const data = e["data"];
  const object: unknown =
    typeof data === "object" && data !== null ? (data as Record<string, unknown>)["object"] : null;
  const { id: eventId, type, account, livemode, created, actor } = e;
  if (
    e["object"] !== "event" ||
    typeof eventId !== "string" ||
    typeof type !== "string" ||
    typeof account !== "string" ||
    typeof livemode !== "boolean" ||
    typeof created !== "number" ||
    !Number.isSafeInteger(created) ||
    typeof object !== "object" ||
    object === null ||
    !(actor === undefined || typeof actor === "string")
  ) {
    throw new TypeError("webhook body is not an event");
  }
  if (eventId !== id) {
    throw new WebhookSignatureError("webhook id does not match the event");
  }
  if (account !== options.expectedAccount) {
    throw new WebhookSignatureError("webhook event is for another account");
  }
  if (livemode !== options.expectedLivemode) {
    throw new WebhookSignatureError("webhook event is for the other mode");
  }
  const previous = (data as Record<string, unknown>)["previous_attributes"];
  return {
    id: eventId,
    object: "event",
    account,
    livemode,
    type,
    created,
    ...(actor === undefined ? {} : { actor }),
    data: {
      object: object as Record<string, unknown>,
      ...(typeof previous === "object" && previous !== null
        ? { previous_attributes: previous as Record<string, unknown> }
        : {}),
    },
  };
}

function header(
  headers: Headers | Record<string, string | string[] | undefined>,
  name: string,
): string | undefined {
  if (headers instanceof Headers) {
    return headers.get(name)?.trim() ?? undefined;
  }
  for (const [key, value] of Object.entries(headers)) {
    if (key.toLowerCase() === name) {
      return (Array.isArray(value) ? value.join(" ") : value)?.trim();
    }
  }
  return undefined;
}

async function importKey(encoded: string): Promise<CryptoKey> {
  const value = encoded.trim().replace(/^0x/, "");
  const raw = /^[0-9a-fA-F]{64}$/.test(value) ? hex(value) : base64(value);
  if (raw?.length !== 32) {
    throw new TypeError("a webhook public key is 32 bytes of hex or base64");
  }
  return crypto.subtle.importKey("raw", raw, "Ed25519", false, ["verify"]);
}

function hex(value: string): Uint8Array<ArrayBuffer> {
  const bytes = new Uint8Array(value.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    bytes[index] = Number.parseInt(value.slice(index * 2, index * 2 + 2), 16);
  }
  return bytes;
}

function base64(value: string): Uint8Array<ArrayBuffer> | undefined {
  if (!/^[A-Za-z0-9+/]*={0,2}$/.test(value) || value.length % 4 !== 0) {
    return undefined;
  }
  return Uint8Array.from(atob(value), (char) => char.charCodeAt(0));
}

function concat(left: Uint8Array, right: Uint8Array): Uint8Array<ArrayBuffer> {
  const joined = new Uint8Array(left.length + right.length);
  joined.set(left);
  joined.set(right, left.length);
  return joined;
}
