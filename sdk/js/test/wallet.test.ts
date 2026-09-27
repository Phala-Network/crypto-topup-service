import { decodeFunctionData, erc20Abi } from "viem";
import { afterEach, describe, expect, it } from "vitest";
import {
  INJECTED_WALLET_UUID,
  WalletError,
  payWithWallet,
  watchWallets,
  type EthereumProvider,
  type Wallet,
} from "../src/index.js";
import { ADDRESS, TOKEN, quote } from "./fixtures.js";

const ACCOUNT = "0x3333333333333333333333333333333333333333";
const HASH = `0x${"ab".repeat(32)}`;

interface Call {
  method: string;
  params?: unknown;
}

/** A wallet on `chainId` that knows `known` chains and fails requests listed in `failures`. */
function mockProvider(chainId: number, known: number[], failures: Record<string, number> = {}) {
  const calls: Call[] = [];
  let current = chainId;
  const provider: EthereumProvider = {
    request: ({ method, params }: Call) => {
      calls.push({ method, params });
      const failure = failures[method];
      if (failure !== undefined) {
        return Promise.reject(Object.assign(new Error(method), { code: failure }));
      }
      switch (method) {
        case "eth_requestAccounts":
          return Promise.resolve([ACCOUNT]);
        case "eth_chainId":
          return Promise.resolve(`0x${current.toString(16)}`);
        case "wallet_switchEthereumChain": {
          const target = Number((params as [{ chainId: string }])[0].chainId);
          if (!known.includes(target)) {
            return Promise.reject(Object.assign(new Error("unknown chain"), { code: 4902 }));
          }
          current = target;
          return Promise.resolve(null);
        }
        case "wallet_addEthereumChain":
          known.push(Number((params as [{ chainId: string }])[0].chainId));
          return Promise.resolve(null);
        case "eth_sendTransaction":
          return Promise.resolve(HASH);
        default:
          return Promise.reject(new Error(`unexpected ${method}`));
      }
    },
  };
  return { provider, calls };
}

function sentTransfer(calls: Call[]) {
  const send = calls.find((c) => c.method === "eth_sendTransaction");
  const [tx] = send?.params as [{ from: string; to: string; data: `0x${string}` }];
  return { tx, decoded: decodeFunctionData({ abi: erc20Abi, data: tx.data }) };
}

describe("payWithWallet", () => {
  it("sends the quote's exact ERC-20 transfer on the quote's chain", async () => {
    const { provider, calls } = mockProvider(11155111, [11155111]);
    await expect(payWithWallet(provider, quote())).resolves.toBe(HASH);
    const { tx, decoded } = sentTransfer(calls);
    expect(tx.from).toBe(ACCOUNT);
    expect(tx.to.toLowerCase()).toBe(TOKEN.toLowerCase());
    expect(decoded).toEqual({
      functionName: "transfer",
      args: [ADDRESS, 100502512562814070352n],
    });
    expect(calls.map((c) => c.method)).not.toContain("wallet_switchEthereumChain");
  });

  it("switches the wallet to the quote's chain", async () => {
    const { provider, calls } = mockProvider(1, [1, 11155111]);
    await payWithWallet(provider, quote());
    expect(calls.map((c) => c.method)).toContain("wallet_switchEthereumChain");
    expect(calls.map((c) => c.method)).not.toContain("wallet_addEthereumChain");
  });

  it("adds a known chain the wallet lacks, then switches", async () => {
    const { provider, calls } = mockProvider(1, [1]);
    await payWithWallet(provider, quote());
    const methods = calls.map((c) => c.method);
    expect(methods.indexOf("wallet_addEthereumChain")).toBeGreaterThan(
      methods.indexOf("wallet_switchEthereumChain"),
    );
    expect(methods.at(-1)).toBe("eth_sendTransaction");
  });

  it("refuses to add a chain it cannot describe", async () => {
    const { provider, calls } = mockProvider(1, [1]);
    const unknownChain = quote({ chain_id: 31337 });
    await expect(payWithWallet(provider, unknownChain)).rejects.toMatchObject({ code: "wrong_chain" });
    expect(calls.map((c) => c.method)).not.toContain("eth_sendTransaction");
  });

  it("reports a rejection in the wallet", async () => {
    const { provider } = mockProvider(11155111, [11155111], { eth_sendTransaction: 4001 });
    const error = await payWithWallet(provider, quote()).catch((e: unknown) => e);
    expect(error).toBeInstanceOf(WalletError);
    expect(error).toMatchObject({ code: "rejected" });
  });

  it("never sends when the payment URI disagrees with the quote", async () => {
    const { provider, calls } = mockProvider(11155111, [11155111]);
    const tampered = quote({ payment_uri: quote({ address: TOKEN }).payment_uri });
    await expect(payWithWallet(provider, tampered)).rejects.toThrow(TypeError);
    expect(calls).toHaveLength(0);
  });
});

describe("watchWallets", () => {
  afterEach(() => {
    delete (window as { ethereum?: unknown }).ethereum;
  });

  it("lists wallets that announce themselves with EIP-6963", () => {
    const { provider } = mockProvider(1, [1]);
    const seen: Wallet[][] = [];
    const onRequest = () => {
      window.dispatchEvent(
        Object.assign(new Event("eip6963:announceProvider"), {
          detail: {
            info: { uuid: "u-1", name: "Test Wallet", icon: "javascript:alert(1)", rdns: "t.w" },
            provider,
          },
        }),
      );
    };
    window.addEventListener("eip6963:requestProvider", onRequest);
    const stop = watchWallets((wallets) => seen.push(wallets));
    window.removeEventListener("eip6963:requestProvider", onRequest);
    stop();

    expect(seen.at(-1)?.map((w) => w.info)).toEqual([
      { uuid: "u-1", name: "Test Wallet", icon: "", rdns: "t.w" },
    ]);
  });

  it("falls back to window.ethereum", () => {
    const { provider } = mockProvider(1, [1]);
    (window as { ethereum?: unknown }).ethereum = provider;
    const seen: Wallet[][] = [];
    watchWallets((wallets) => seen.push(wallets))();
    expect(seen.at(-1)?.map((w) => w.info.uuid)).toEqual([INJECTED_WALLET_UUID]);
  });
});
