import { getAddress, isAddressEqual, type Address } from "viem";
import type { ClientQuote } from "./quote.js";

/** An ERC-20 transfer request, as EIP-681 encodes it and a wallet sends it. */
export interface TokenTransfer {
  chainId: number;
  /** The token contract. */
  token: Address;
  /** The recipient: the quote's deposit address. */
  to: Address;
  /** Token amount in the smallest unit. */
  amount: bigint;
}

const EIP681_TRANSFER =
  /^ethereum:(?:pay-)?(0x[0-9a-fA-F]{40})@([1-9][0-9]*)\/transfer\?([^#]*)$/;

/**
 * Reads the token transfer from a quote's EIP-681 `payment_uri`, and checks that it pays exactly
 * the quote's `amount_atomic` to the quote's `address` on the quote's chain, so the wallet, the QR
 * code, and the manual instructions all state the same payment.
 */
export function quoteTransfer(quote: ClientQuote): TokenTransfer {
  const match = EIP681_TRANSFER.exec(quote.payment_uri);
  const params = new URLSearchParams(match?.[3] ?? "");
  const to = params.get("address");
  const amount = params.get("uint256");
  if (
    match?.[1] === undefined ||
    match[2] === undefined ||
    to === null ||
    !/^0x[0-9a-fA-F]{40}$/.test(to) ||
    amount === null ||
    !/^\d+$/.test(amount)
  ) {
    throw new TypeError("payment_uri is not an EIP-681 ERC-20 transfer");
  }
  const transfer: TokenTransfer = {
    chainId: Number(match[2]),
    token: getAddress(match[1]),
    to: getAddress(to),
    amount: BigInt(amount),
  };
  if (
    transfer.chainId !== quote.chain_id ||
    !isAddressEqual(transfer.to, getAddress(quote.address)) ||
    transfer.amount !== BigInt(quote.amount_atomic)
  ) {
    throw new TypeError("payment_uri does not match the quote");
  }
  return transfer;
}
