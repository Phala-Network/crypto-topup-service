// @vitest-environment node
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import {
  WebhookSignatureError,
  batchChecksum,
  constructEvent,
  depositAddress,
  flushTransaction,
  flushTransactions,
  forwarderAddress,
  quoteSalt,
  quoteAddress,
  safeBatch,
  type BatchFile,
} from "../src/server/index.js";

// Tests run from sdk/js.
const repo = (path: string) => resolve(process.cwd(), "../..", path);
const vectors = JSON.parse(
  readFileSync(repo("contracts/test-vectors/create2.json"), "utf8"),
) as {
  factory: string;
  implementation: string;
  quote: { account: string; client_reference_id: string; quote_id: string; salt: `0x${string}`; treasury: string; predicted_address: string }[];
  deposit_address: {
    account: string;
    livemode: boolean;
    client_reference_id: string;
    version: number;
    treasury: string;
    predicted_address: string;
  }[];
};
const forwarder = { factory: vectors.factory, implementation: vectors.implementation };

describe("address recomputation", () => {
  it("reproduces the contract vectors of quotes", () => {
    expect(vectors.quote.length).toBeGreaterThan(0);
    for (const vector of vectors.quote) {
      expect(quoteSalt(vector.account, vector.client_reference_id, vector.quote_id)).toBe(vector.salt);
      expect(forwarderAddress(vectors.factory, vectors.implementation, vector.treasury, vector.salt)).toBe(
        vector.predicted_address,
      );
      const quote = { treasury: vector.treasury, client_reference_id: vector.client_reference_id, id: vector.quote_id };
      expect(quoteAddress(forwarder, quote, vector.account)).toBe(vector.predicted_address);
    }
  });

  it("reproduces the contract vectors of deposit addresses", () => {
    expect(vectors.deposit_address.length).toBeGreaterThan(0);
    for (const vector of vectors.deposit_address) {
      expect(depositAddress(forwarder, vector, vector.treasury, vector.account)).toBe(
        vector.predicted_address,
      );
    }
  });
});

// The Rust core's and the Python SDK's signing vector (sdk/python/tests/test_webhooks.py): the
// seed [7; 32] signs `{id}.{timestamp}.{body}`.
const PUBLIC_KEY = "ea4a6c63e29c520abef5507b132ec5f9954776aebebe7b92421eea691446d22c";
const RUST_ID = "018d5f8e-8a7b-7d65-bc44-2c4f5f0a6d31";
const RUST_TIMESTAMP = 1_674_087_231;
const RUST_BODY = '{"type":"deposit.confirmed","data":{"deposit_id":"dep_123"}}';
const RUST_SIGNATURE =
  "v1a,0thypM6abf9ly803QGttAKGQfPFKHiwgpxF+b4zWDUCycKswAoJ848WmI7VKQBw8NIWO74zYeRvd7vw/cGOZBw==";

async function signed(body: string, id: string) {
  const pair = await crypto.subtle.generateKey("Ed25519", true, ["sign", "verify"]);
  const timestamp = Math.floor(Date.now() / 1000);
  const signature = await crypto.subtle.sign(
    "Ed25519",
    pair.privateKey,
    new TextEncoder().encode(`${id}.${timestamp}.${body}`),
  );
  const raw = new Uint8Array(await crypto.subtle.exportKey("raw", pair.publicKey));
  return {
    publicKey: Buffer.from(raw).toString("base64"),
    headers: {
      "webhook-id": id,
      "webhook-timestamp": String(timestamp),
      "webhook-signature": `v1a,${Buffer.from(signature).toString("base64")}`,
    },
  };
}

const ACCOUNT = `acct_${"a1".repeat(16)}`;
const EVENT_ID = `evt_${"26".repeat(16)}`;
function event(overrides: Record<string, unknown> = {}) {
  return JSON.stringify({
    id: EVENT_ID,
    object: "event",
    account: ACCOUNT,
    livemode: false,
    type: "deposit.credited",
    created: 1_790_000_000,
    data: { object: { id: `dep_${"01".repeat(16)}`, object: "deposit" } },
    ...overrides,
  });
}

describe("constructEvent", () => {
  it("verifies the service's v1a signature vector", async () => {
    const options = { expectedAccount: ACCOUNT, expectedLivemode: false, now: RUST_TIMESTAMP };
    const headers = new Headers({
      "Webhook-Id": RUST_ID,
      "Webhook-Timestamp": String(RUST_TIMESTAMP),
      "Webhook-Signature": RUST_SIGNATURE,
    });
    // The signature verifies; the vector's body is no event, which is refused after it.
    await expect(constructEvent(RUST_BODY, headers, PUBLIC_KEY, options)).rejects.toThrow(
      "not an event",
    );
    await expect(
      constructEvent(RUST_BODY.replace("123", "124"), headers, PUBLIC_KEY, options),
    ).rejects.toBeInstanceOf(WebhookSignatureError);
  });

  it("returns the event of the expected account and mode", async () => {
    const body = event();
    const { publicKey, headers } = await signed(body, EVENT_ID);
    const verified = await constructEvent(body, headers, [PUBLIC_KEY, publicKey], {
      expectedAccount: ACCOUNT,
      expectedLivemode: false,
    });
    expect(verified.type).toBe("deposit.credited");
    expect(verified.data.object["object"]).toBe("deposit");
  });

  it.each([
    ["another account", event({ account: `acct_${"b2".repeat(16)}` }), EVENT_ID, "another account"],
    ["the other mode", event({ livemode: true }), EVENT_ID, "other mode"],
    ["another id", event(), `evt_${"00".repeat(16)}`, "does not match"],
  ])("fails closed for %s", async (_, body, id, message) => {
    const { publicKey, headers } = await signed(body, id);
    await expect(
      constructEvent(body, headers, publicKey, { expectedAccount: ACCOUNT, expectedLivemode: false }),
    ).rejects.toThrow(message);
  });

  it("refuses a stale, unsigned, or foreign delivery", async () => {
    const body = event();
    const { publicKey, headers } = await signed(body, EVENT_ID);
    const options = { expectedAccount: ACCOUNT, expectedLivemode: false };
    await expect(
      constructEvent(body, headers, publicKey, { ...options, now: Date.now() / 1000 + 301 }),
    ).rejects.toThrow("tolerance");
    await expect(constructEvent(body, {}, publicKey, options)).rejects.toThrow("headers missing");
    await expect(constructEvent(body, headers, PUBLIC_KEY, options)).rejects.toThrow(
      "no valid webhook signature",
    );
    await expect(
      constructEvent(body, headers, publicKey, { ...options, expectedAccount: "" }),
    ).rejects.toThrow(TypeError);
  });
});

const FACTORY = "0xe8A9Ab1AbC7651A5b7C2ED5B662F2f80BF5C446d";
const TREASURY = "0x0000000000000000000000000000000000007EA5";
const SAFE = "0xDF8a1Ce35c9a6ACE153B4e0767942f1E2291a1Aa";
const TOKEN = "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E";
const salt = (byte: string) => `0x${byte.repeat(32)}` as const;

describe("sweeps", () => {
  // The batch file the Python SDK writes for the same calls (sdk/python/tests/test_sweeps.py).
  const fixture: BatchFile = JSON.parse(
    readFileSync(repo("sdk/testdata/safe-batch.json"), "utf8"),
  ) as BatchFile;

  it("writes the same Transaction Builder batch file as the Python SDK", () => {
    const calls = [
      flushTransaction(FACTORY, TREASURY, [salt("01"), salt("02")], TOKEN),
      flushTransaction(FACTORY, TREASURY, [salt("03")], TOKEN),
    ];
    const batch: BatchFile = safeBatch(1, SAFE, calls, {
      name: "Phala Pay sweep",
      createdAt: 1_790_000_000_000,
    });
    expect(batch).toEqual(fixture);
    // The app's validateChecksum: drop the checksum, recompute, compare.
    expect(batchChecksum(fixture)).toBe(fixture.meta.checksum);
    expect(batchChecksum({ ...fixture, chainId: "10" })).not.toBe(fixture.meta.checksum);
  });

  it("groups sweepable forwarders into one flush per treasury", () => {
    const other = `0x${"99".repeat(20)}`;
    const forwarders = [
      { chain_id: 1, factory: FACTORY, treasury: TREASURY, salt: salt("01") },
      { chain_id: 1, factory: FACTORY, treasury: other, salt: salt("02") },
      { chain_id: 1, factory: FACTORY, treasury: TREASURY, salt: salt("03") },
    ];
    expect(flushTransactions(forwarders, TOKEN)).toEqual([
      flushTransaction(FACTORY, TREASURY, [salt("01"), salt("03")], TOKEN),
      flushTransaction(FACTORY, other, [salt("02")], TOKEN),
    ]);
    const otherChain = { chain_id: 10, factory: FACTORY, treasury: TREASURY, salt: salt("04") };
    expect(() => flushTransactions([...forwarders, otherChain], TOKEN)).toThrow(
      "one chain",
    );
    expect(() => flushTransaction(FACTORY, TREASURY, [], TOKEN)).toThrow("at least one salt");
  });
});
