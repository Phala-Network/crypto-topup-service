import { WalletError, knownChain, networkName, watchWallets, type Wallet } from "@phala/pay";
import {
  createWalletClient,
  custom,
  erc20Abi,
  getAddress,
  isHex,
  parseAbi,
  type Account,
  type Address,
  type Chain,
  type Hash,
  type WalletClient,
} from "viem";
import { readContract, waitForTransactionReceipt } from "viem/actions";
import { tokens } from "./format.js";

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
  account: Account | Address;
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

/** Mints `amountAtomic` of a test token to the visitor's wallet; resolves once it is in a block. */
export async function mintTestTokens(chainId: number, token: string, amountAtomic: bigint): Promise<Hash> {
  const { client, account, chain } = await connect(chainId);
  const hash = await client.writeContract({
    account,
    chain,
    address: getAddress(token),
    abi: MINT_ABI,
    functionName: "mint",
    args: [addressOf(account), amountAtomic],
  });
  // So that a payment right after it sees the minted balance.
  const { status } = await waitForTransactionReceipt(client, { hash });
  if (status !== "success") {
    throw new Error("The mint transaction failed.");
  }
  return hash;
}

/**
 * An ERC-20 transfer from the visitor's wallet. A wallet holding less than the amount sends
 * nothing (the transfer would revert and still cost gas): `WalletError` `insufficient_balance`, as
 * the SDK's checkout refuses it.
 */
export async function transferTokens(
  chainId: number,
  token: { contract: string; symbol: string; decimals: number },
  to: string,
  amountAtomic: bigint,
): Promise<Hash> {
  const { client, account, chain } = await connect(chainId);
  const contract = getAddress(token.contract);
  const balance = await readContract(client, {
    address: contract,
    abi: erc20Abi,
    functionName: "balanceOf",
    args: [addressOf(account)],
  });
  if (balance < amountAtomic) {
    const held = tokens(balance.toString(), token.symbol, token.decimals);
    const needed = tokens(amountAtomic.toString(), token.symbol, token.decimals);
    throw new WalletError("insufficient_balance", `Your wallet holds ${held}, less than the ${needed} to send. Nothing was sent.`);
  }
  return client.writeContract({
    account,
    chain,
    address: contract,
    abi: erc20Abi,
    functionName: "transfer",
    args: [getAddress(to), amountAtomic],
  });
}

function addressOf(account: Account | Address): Address {
  return typeof account === "string" ? account : account.address;
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
