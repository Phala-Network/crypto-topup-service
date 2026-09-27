import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { PhalaPay, checkoutStatus, createCheckout, type CheckoutState } from "../src/index.js";
import { API_BASE, CLIENT_SECRET, QUOTE_ID, fakeFetch, quote } from "./fixtures.js";

const NOW = (quote().expires_at - 600) * 1000;

beforeEach(() => {
  vi.useFakeTimers({ now: NOW });
});
afterEach(() => {
  vi.useRealTimers();
});

function start(fetch: typeof globalThis.fetch) {
  const states: CheckoutState[] = [];
  const checkout = createCheckout({
    clientSecret: CLIENT_SECRET,
    apiBase: `${API_BASE}/`,
    fetch,
    pollInterval: 1000,
  });
  checkout.subscribe((state) => states.push(state));
  return { checkout, states };
}

describe("createCheckout", () => {
  it("polls the public view until the payment is credited", async () => {
    const { fetch, calls } = fakeFetch(
      quote(),
      quote({ payment_status: "seen", confirmations: 1 }),
      quote({ payment_status: "confirming" }),
      quote({ status: "complete", payment_status: "credited" }),
    );
    const { states } = start(fetch);
    await vi.advanceTimersByTimeAsync(10_000);

    expect(states.map((s) => s.status)).toEqual(["waiting", "seen", "confirming", "credited"]);
    expect(calls).toHaveLength(4);
    expect(calls[0]).toBe(
      `${API_BASE}/v1/quotes/${QUOTE_ID}?client_secret=${encodeURIComponent(CLIENT_SECRET)}`,
    );
  });

  it("does not notify when nothing changed", async () => {
    const { fetch, calls } = fakeFetch(quote());
    const { states } = start(fetch);
    await vi.advanceTimersByTimeAsync(5_000);
    expect(calls.length).toBeGreaterThan(3);
    expect(states).toHaveLength(1);
  });

  it("stops with an error on an unknown client secret", async () => {
    const { fetch, calls } = fakeFetch(404);
    const { checkout } = start(fetch);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(checkout.getState()).toMatchObject({
      status: "error",
      error: { code: "invalid_client_secret" },
    });
    expect(calls).toHaveLength(1);
  });

  it("keeps the last quote and backs off while the service is unreachable", async () => {
    const { fetch, calls } = fakeFetch(quote(), new TypeError("offline"), 503, quote());
    const { checkout, states } = start(fetch);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(checkout.getState()).toMatchObject({ status: "waiting", error: { code: "network_error" } });
    await vi.advanceTimersByTimeAsync(2_000);
    expect(checkout.getState()).toMatchObject({ status: "waiting", error: { code: "api_error" } });
    // The third failure-free read waits 2^2 intervals after two failures.
    await vi.advanceTimersByTimeAsync(3_999);
    expect(calls).toHaveLength(3);
    await vi.advanceTimersByTimeAsync(1);
    expect(checkout.getState()).toMatchObject({ status: "waiting", error: null });
    expect(states.every((s) => s.quote !== null)).toBe(true);
  });

  it("turns expired at expires_at and stops once the service expires the quote", async () => {
    const { fetch, calls } = fakeFetch(quote(), quote(), quote({ status: "expired" }));
    const { checkout } = start(fetch);
    await vi.advanceTimersByTimeAsync(0);
    vi.setSystemTime(quote().expires_at * 1000);
    await vi.advanceTimersByTimeAsync(1_000);
    expect(checkout.getState().status).toBe("expired");
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls).toHaveLength(3);
  });

  it("rejects an invalid response body", async () => {
    const { fetch } = fakeFetch({ ...quote(), status: "unknown" } as never);
    const { checkout } = start(fetch);
    await vi.advanceTimersByTimeAsync(0);
    expect(checkout.getState()).toMatchObject({
      status: "loading",
      error: { code: "invalid_response" },
    });
  });

  it("stops polling when destroyed", async () => {
    const { fetch, calls } = fakeFetch(quote());
    const { checkout } = start(fetch);
    await vi.advanceTimersByTimeAsync(0);
    checkout.destroy();
    await vi.advanceTimersByTimeAsync(10_000);
    expect(calls).toHaveLength(1);
  });
});

describe("checkoutStatus", () => {
  const now = quote().expires_at - 1;
  it.each([
    [quote(), "waiting"],
    [quote({ payment_status: "seen" }), "seen"],
    [quote({ payment_status: "rejected" }), "rejected"],
    [quote({ status: "canceled" }), "canceled"],
    [quote({ status: "expired" }), "expired"],
    [quote({ expires_at: now }), "expired"],
    // A payment seen after local expiry still shows.
    [quote({ expires_at: now, payment_status: "seen" }), "seen"],
  ] as const)("%# is %s", (value, expected) => {
    expect(checkoutStatus(value, now)).toBe(expected);
  });
});

describe("PhalaPay", () => {
  it("retrieves the public view with the client secret", async () => {
    const { fetch, calls } = fakeFetch(quote());
    const pay = new PhalaPay({ apiBase: API_BASE, fetch });
    await expect(pay.retrieveQuote(CLIENT_SECRET)).resolves.toEqual(quote());
    expect(calls[0]).toBe(
      `${API_BASE}/v1/quotes/${QUOTE_ID}?client_secret=${encodeURIComponent(CLIENT_SECRET)}`,
    );
  });

  it("rejects an unknown client secret", async () => {
    const pay = new PhalaPay({ apiBase: API_BASE, fetch: fakeFetch(404).fetch });
    await expect(pay.retrieveQuote(CLIENT_SECRET)).rejects.toMatchObject({
      code: "invalid_client_secret",
    });
  });

  it("follows a checkout session", async () => {
    const pay = new PhalaPay({ apiBase: API_BASE, fetch: fakeFetch(quote()).fetch });
    const session = pay.checkout(CLIENT_SECRET, { pollInterval: 1000 });
    await vi.advanceTimersByTimeAsync(0);
    expect(session.getState().status).toBe("waiting");
    session.destroy();
  });
});
