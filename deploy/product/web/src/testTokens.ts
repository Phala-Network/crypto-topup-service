import { knownChain, networkName, watchWallets, type Wallet } from "@phala/pay";
import { createWalletClient, custom, getAddress, parseUnits, type Hash } from "viem";

// Staging's test PHA is a MockERC20 whose `mint(address,uint256)` is public: the visitor's own
// wallet mints, so the demo holds no faucet key. Gas is Sepolia ETH from any public faucet.
const MINT_ABI = [
  {
    type: "function",
    name: "mint",
    stateMutability: "nonpayable",
    inputs: [
      { name: "account", type: "address" },
      { name: "amount", type: "uint256" },
    ],
    outputs: [],
  },
] as const;

/** The first wallet the browser announces within 300 ms (EIP-6963, or `window.ethereum`). */
export function firstWallet(): Promise<Wallet | undefined> {
  return new Promise((resolve) => {
    let found: Wallet | undefined;
    const stop = watchWallets((wallets) => {
      found ??= wallets[0];
    });
    setTimeout(() => {
      stop();
      resolve(found);
    }, 300);
  });
}

export async function mintTestTokens(
  wallet: Wallet,
  chainId: number,
  token: string,
  amount: string,
): Promise<Hash> {
  const chain = knownChain(chainId);
  if (chain === undefined) {
    throw new Error(`Unsupported network ${networkName(chainId)}`);
  }
  const client = createWalletClient({ transport: custom(wallet.provider) });
  const [account] = await client.requestAddresses();
  if (account === undefined) {
    throw new Error("The wallet shared no account");
  }
  if ((await client.getChainId()) !== chainId) {
    try {
      await client.switchChain({ id: chainId });
    } catch {
      await client.addChain({ chain });
      await client.switchChain({ id: chainId });
    }
  }
  return client.writeContract({
    account,
    chain,
    address: getAddress(token),
    abi: MINT_ABI,
    functionName: "mint",
    args: [account, parseUnits(amount, 18)],
  });
}
