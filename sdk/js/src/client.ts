import {
  createCheckout,
  retrieveQuote,
  type CheckoutOptions,
  type CheckoutSession,
} from "./checkout.js";
import type { ClientQuote } from "./quote.js";

export interface PhalaPayOptions {
  /** The service origin, for example `https://pay.example.com`. */
  apiBase: string;
  fetch?: typeof globalThis.fetch;
}

/**
 * The browser client, in the shape of Stripe.js: it holds no key, only the service origin, and
 * works with the `client_secret` your backend got from `POST /v1/quotes` and the `address` its
 * SDK recomputed (`expectedAddress`).
 *
 *     const pay = new PhalaPay({ apiBase: "https://pay.example.com" });
 *     const quote = await pay.retrieveQuote(clientSecret, expectedAddress);
 *     const session = pay.checkout(clientSecret, { expectedAddress });
 *     session.subscribe(({ status }) => render(status));
 */
export class PhalaPay {
  readonly apiBase: string;
  readonly #fetch: typeof globalThis.fetch | undefined;

  constructor(options: PhalaPayOptions) {
    this.apiBase = options.apiBase;
    this.#fetch = options.fetch;
  }

  /** Reads the quote's public view once, refusing one whose address is not `expectedAddress`. */
  retrieveQuote(clientSecret: string, expectedAddress: string): Promise<ClientQuote> {
    return retrieveQuote({
      clientSecret,
      expectedAddress,
      apiBase: this.apiBase,
      ...this.#fetchOption(),
    });
  }

  /** Follows the quote until it is credited, rejected, reversed, canceled, or expired. */
  checkout(
    clientSecret: string,
    options: Pick<CheckoutOptions, "expectedAddress" | "pollInterval">,
  ): CheckoutSession {
    return createCheckout({
      clientSecret,
      apiBase: this.apiBase,
      ...options,
      ...this.#fetchOption(),
    });
  }

  #fetchOption(): { fetch?: typeof globalThis.fetch } {
    return this.#fetch === undefined ? {} : { fetch: this.#fetch };
  }
}
