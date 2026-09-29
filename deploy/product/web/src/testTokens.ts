import { knownChain, networkName, watchWallets, type Wallet } from "@phala/pay";
import {
  createWalletClient,
  custom,
  erc20Abi,
  getAddress,
  isHex,
  parseAbi,
  parseUnits,
  type Account,
  type Chain,
  type Hash,
  type WalletClient,
} from "viem";

// Staging's test PHA, on each network, is a MockERC20 whose `mint(address,uint256)` is public: the
// visitor's own wallet mints, so the demo holds no faucet key. Gas is the testnet's ETH from any
// public faucet. The
// same wallet pays to the deposit address, sends a sweep (a flush is permissionless), and may pay
// a refund, which fails verification unless it comes from the treasury.
const MINT_ABI = parseAbi(["function mint(address account, uint256 amount)"]);

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

interface Connected {
  client: WalletClient;
  account: Account | `0x${string}`;
  chain: Chain;
}

/** The first browser wallet, its first account, switched to `chainId`. */
async function connect(chainId: number): Promise<Connected> {
  const chain = knownChain(chainId);
  if (chain === undefined) {
    throw new Error(`Unsupported network ${networkName(chainId)}`);
  }
  const wallet = await firstWallet();
  if (wallet === undefined) {
    throw new Error("No browser wallet found.");
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
  return { client, account, chain };
}

export async function mintTestTokens(chainId: number, token: string, amount: string, decimals = 18): Promise<Hash> {
  const { client, account, chain } = await connect(chainId);
  const to = typeof account === "string" ? account : account.address;
  return client.writeContract({
    account,
    chain,
    address: getAddress(token),
    abi: MINT_ABI,
    functionName: "mint",
    args: [to, parseUnits(amount, decimals)],
  });
}

/** An ERC-20 transfer from the visitor's wallet. */
export async function transferTokens(
  chainId: number,
  token: string,
  to: string,
  amountAtomic: bigint,
): Promise<Hash> {
  const { client, account, chain } = await connect(chainId);
  return client.writeContract({
    account,
    chain,
    address: getAddress(token),
    abi: erc20Abi,
    functionName: "transfer",
    args: [getAddress(to), amountAtomic],
  });
}

/** Sends a prepared call, such as the SDK's `factory.flush(treasury, salts, token)`. */
export async function sendCall(chainId: number, call: { to: string; data: string; value: string }): Promise<Hash> {
  if (!isHex(call.data)) {
    throw new Error("The call's data is not hex");
  }
  const { client, account, chain } = await connect(chainId);
  return client.sendTransaction({
    account,
    chain,
    to: getAddress(call.to),
    data: call.data,
    value: BigInt(call.value),
  });
}
