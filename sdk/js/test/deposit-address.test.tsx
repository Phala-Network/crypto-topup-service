import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, describe, expect, it } from "vitest";
import {
  depositAddressTransfer,
  type DepositAddressAsset,
  type DepositAddressDetails,
  type DepositAddressNetwork,
} from "../src/index.js";
import { DepositAddress } from "../src/react/index.js";
import { ADDRESS, TOKEN } from "./fixtures.js";

const USDC = "0x036CbD53842c5426634e7929541eC2318f3dCF7e";
const OTHER = `0x${"2".repeat(40)}`;

function asset(chainId: number, symbol: string, contract: string, to = ADDRESS): DepositAddressAsset {
  return {
    asset: symbol,
    contract,
    decimals: 18,
    payment_uri: `ethereum:${contract}@${chainId}/transfer?address=${to}`,
  };
}

function network(chainId: number, assets: DepositAddressAsset[], address = ADDRESS): DepositAddressNetwork {
  return { chain_id: chainId, address, assets };
}

function details(overrides: Partial<DepositAddressDetails> = {}): DepositAddressDetails {
  return {
    address: ADDRESS,
    networks: [
      network(11155111, [asset(11155111, "pha", TOKEN), asset(11155111, "usdc", USDC)]),
      network(84532, [asset(84532, "usdc", USDC)]),
    ],
    ...overrides,
  };
}

afterEach(cleanup);

describe("depositAddressTransfer", () => {
  it("reads one token's transfer request, which names no amount", () => {
    const sepolia = network(11155111, [asset(11155111, "pha", TOKEN)]);
    expect(depositAddressTransfer(sepolia, asset(11155111, "pha", TOKEN))).toEqual({
      chainId: 11155111,
      token: TOKEN,
      to: ADDRESS,
      amount: undefined,
    });
  });

  it.each([
    ["another recipient", network(11155111, [], OTHER), asset(11155111, "pha", TOKEN)],
    ["another chain", network(1, []), asset(11155111, "pha", TOKEN)],
    ["another token", network(11155111, []), { ...asset(11155111, "pha", TOKEN), contract: USDC }],
    [
      "an amount",
      network(11155111, []),
      { ...asset(11155111, "pha", TOKEN), payment_uri: `ethereum:${TOKEN}@11155111/transfer?address=${ADDRESS}&uint256=1` },
    ],
    [
      "a native transfer",
      network(11155111, []),
      { ...asset(11155111, "pha", TOKEN), payment_uri: `ethereum:${ADDRESS}@11155111?value=1` },
    ],
  ])("refuses a payment URI with %s", (_, onNetwork, token) => {
    expect(() => depositAddressTransfer(onNetwork, token)).toThrow(TypeError);
  });
});

describe("DepositAddress", () => {
  it("shows one address for every network and token, with a QR code per network and token", () => {
    render(<DepositAddress depositAddress={details()} />);
    expect(screen.getByText("One address for all supported tokens and networks")).toBeDefined();
    const qr = () => screen.getByRole("img");
    expect(qr().getAttribute("aria-label")).toBe("Deposit address for PHA on Sepolia");
    expect(qr().querySelector("path")?.getAttribute("d")).toMatch(/^M\d+ \d+h1v1h-1z/);
    expect(screen.getByText("Sepolia (chain ID 11155111)")).toBeDefined();
    expect(screen.getByText(ADDRESS)).toBeDefined();
    expect(screen.getByText(TOKEN)).toBeDefined();
    expect(screen.getByRole("button", { name: "Copy Deposit address" })).toBeDefined();
    expect(screen.getByText(/Send only PHA, USDC on Sepolia, Base Sepolia/)).toBeDefined();

    fireEvent.click(screen.getByRole("tab", { name: "USDC" }));
    expect(qr().getAttribute("aria-label")).toBe("Deposit address for USDC on Sepolia");
    expect(screen.getByText(USDC)).toBeDefined();

    fireEvent.click(screen.getByRole("tab", { name: "Base Sepolia" }));
    expect(qr().getAttribute("aria-label")).toBe("Deposit address for USDC on Base Sepolia");
    expect(screen.getByText("Base Sepolia (chain ID 84532)")).toBeDefined();
    // One token on this network: no token tabs.
    expect(screen.queryByRole("tablist", { name: "Token" })).toBeNull();
  });

  it("shows each network's own address when they differ", () => {
    render(
      <DepositAddress
        chainId={84532}
        depositAddress={details({
          address: null,
          networks: [
            network(11155111, [asset(11155111, "pha", TOKEN)]),
            network(84532, [asset(84532, "usdc", USDC, OTHER)], OTHER),
          ],
        })}
      />,
    );
    expect(screen.getByText(/it differs on some networks/)).toBeDefined();
    expect(screen.getByText(OTHER)).toBeDefined();
  });

  it("refuses details whose payment URI pays another address", () => {
    expect(() =>
      render(
        <DepositAddress
          depositAddress={details({ networks: [network(11155111, [asset(11155111, "pha", TOKEN)], OTHER)] })}
        />,
      ),
    ).toThrow(TypeError);
  });
});
