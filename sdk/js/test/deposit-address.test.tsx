import { cleanup, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import { depositAddressTransfer, type DepositAddressDetails } from "../src/index.js";
import { DepositAddress } from "../src/react/index.js";
import { ADDRESS, TOKEN } from "./fixtures.js";

function details(overrides: Partial<DepositAddressDetails> = {}): DepositAddressDetails {
  return {
    address: ADDRESS,
    chain_id: 11155111,
    asset: "pha",
    payment_uri: `ethereum:${TOKEN}@11155111/transfer?address=${ADDRESS}`,
    ...overrides,
  };
}

afterEach(cleanup);

describe("depositAddressTransfer", () => {
  it("reads the transfer request, which names no amount", () => {
    expect(depositAddressTransfer(details())).toEqual({
      chainId: 11155111,
      token: TOKEN,
      to: ADDRESS,
      amount: undefined,
    });
  });

  it.each([
    ["another recipient", details({ address: `0x${"2".repeat(40)}` })],
    ["another chain", details({ chain_id: 1 })],
    ["an amount", details({ payment_uri: `ethereum:${TOKEN}@11155111/transfer?address=${ADDRESS}&uint256=1` })],
    ["a native transfer", details({ payment_uri: `ethereum:${ADDRESS}@11155111?value=1` })],
  ])("refuses a payment URI with %s", (_, value) => {
    expect(() => depositAddressTransfer(value)).toThrow(TypeError);
  });
});

describe("DepositAddress", () => {
  it("shows the network, a QR code, and the address and token to copy", () => {
    render(<DepositAddress depositAddress={details()} />);
    expect(screen.getByText("Send PHA on Sepolia")).toBeDefined();
    const qr = screen.getByRole("img");
    expect(qr.getAttribute("aria-label")).toBe("Deposit address for PHA on Sepolia");
    expect(qr.querySelector("path")?.getAttribute("d")).toMatch(/^M\d+ \d+h1v1h-1z/);
    expect(screen.getByText("Sepolia (chain ID 11155111)")).toBeDefined();
    expect(screen.getByText(ADDRESS)).toBeDefined();
    expect(screen.getByText(TOKEN)).toBeDefined();
    expect(screen.getByRole("button", { name: "Copy Deposit address" })).toBeDefined();
    expect(screen.getByText(/credited at the market rate when it arrives/)).toBeDefined();
  });

  it("refuses details whose payment URI pays another address", () => {
    expect(() =>
      render(<DepositAddress depositAddress={details({ address: `0x${"2".repeat(40)}` })} />),
    ).toThrow(TypeError);
  });
});
