import {
  createWalletClient,
  custom,
  erc20Abi,
  type Hash,
  type WalletClient,
} from "viem";
import { knownChain, networkName } from "./chains.js";
import { quoteTransfer } from "./payment.js";
import type { ClientQuote } from "./quote.js";

/** EIP-6963 provider info. */
export interface WalletInfo {
  uuid: string;
  name: string;
  /** A data URI. */
  icon: string;
  rdns: string;
}

/** An EIP-1193 provider; viem's `EIP1193Provider` and every injected wallet satisfy it. */
export interface EthereumProvider {
  request(args: { method: string; params?: unknown }): Promise<unknown>;
}

export interface Wallet {
  info: WalletInfo;
  provider: EthereumProvider;
}

/** The legacy `window.ethereum` provider, offered when no wallet announces itself (EIP-6963). */
export const INJECTED_WALLET_UUID = "injected";

interface AnnounceProviderEvent extends Event {
  detail?: { info?: Partial<WalletInfo>; provider?: EthereumProvider };
}

/**
 * Discovers browser wallets with EIP-6963, falling back to `window.ethereum`, and calls `onChange`
 * with the current list whenever a wallet announces itself. Returns the function that stops
 * listening.
 */
export function watchWallets(onChange: (wallets: Wallet[]) => void): () => void {
  if (typeof window === "undefined") {
    onChange([]);
    return () => undefined;
  }
  const announced = new Map<string, Wallet>();
  const emit = () => {
    const wallets = [...announced.values()];
    const injected = (window as { ethereum?: EthereumProvider }).ethereum;
    if (wallets.length === 0 && injected !== undefined) {
      wallets.push({
        info: { uuid: INJECTED_WALLET_UUID, name: "Browser wallet", icon: "", rdns: "" },
        provider: injected,
      });
    }
    onChange(wallets);
  };
  const onAnnounce = (event: Event) => {
    const detail = (event as AnnounceProviderEvent).detail;
    const info = detail?.info;
    if (
      detail?.provider === undefined ||
      typeof info?.uuid !== "string" ||
      typeof info.name !== "string"
    ) {
      return;
    }
    announced.set(info.uuid, {
      info: {
        uuid: info.uuid,
        name: info.name,
        icon: typeof info.icon === "string" && info.icon.startsWith("data:image/") ? info.icon : "",
        rdns: typeof info.rdns === "string" ? info.rdns : "",
      },
      provider: detail.provider,
    });
    emit();
  };
  window.addEventListener("eip6963:announceProvider", onAnnounce);
  window.dispatchEvent(new Event("eip6963:requestProvider"));
  emit();
  return () => window.removeEventListener("eip6963:announceProvider", onAnnounce);
}

export type WalletErrorCode = "rejected" | "no_account" | "wrong_chain" | "failed";

export class WalletError extends Error {
  override readonly name = "WalletError";

  constructor(
    readonly code: WalletErrorCode,
    message: string,
    options?: ErrorOptions,
  ) {
    super(message, options);
  }
}

/**
 * Pays a quote from a browser wallet: connects, switches to (or adds) the quote's chain, and sends
 * the ERC-20 `transfer` that the quote's `payment_uri` states. Resolves with the transaction hash
 * once the wallet has broadcast it; the checkout's status follows the payment from there.
 */
export async function payWithWallet(provider: EthereumProvider, quote: ClientQuote): Promise<Hash> {
  const transfer = quoteTransfer(quote);
  const wallet = createWalletClient({ transport: custom(provider) });
  try {
    const [account] = await wallet.requestAddresses();
    if (account === undefined) {
      throw new WalletError("no_account", "The wallet shared no account");
    }
    await ensureChain(wallet, transfer.chainId);
    return await wallet.writeContract({
      account,
      chain: knownChain(transfer.chainId) ?? null,
      address: transfer.token,
      abi: erc20Abi,
      functionName: "transfer",
      args: [transfer.to, transfer.amount],
    });
  } catch (error) {
    if (error instanceof WalletError) {
      throw error;
    }
    if (hasCode(error, 4001)) {
      throw new WalletError("rejected", "The request was rejected in the wallet", { cause: error });
    }
    throw new WalletError("failed", "The wallet could not send the payment", { cause: error });
  }
}

async function ensureChain(wallet: WalletClient, chainId: number): Promise<void> {
  if ((await wallet.getChainId()) === chainId) {
    return;
  }
  try {
    await wallet.switchChain({ id: chainId });
  } catch (error) {
    // 4902: the wallet does not know the chain yet.
    const chain = knownChain(chainId);
    if (!hasCode(error, 4902) || chain === undefined) {
      throw hasCode(error, 4001)
        ? error
        : new WalletError("wrong_chain", `Switch your wallet to ${networkName(chainId)}`, {
            cause: error,
          });
    }
    await wallet.addChain({ chain });
    if ((await wallet.getChainId()) !== chainId) {
      await wallet.switchChain({ id: chainId });
    }
  }
  if ((await wallet.getChainId()) !== chainId) {
    throw new WalletError("wrong_chain", `Switch your wallet to ${networkName(chainId)}`);
  }
}

/** Whether an error or any of its causes carries an EIP-1193 error code; some mobile wallets nest
 * the original error under `data.originalError`. */
function hasCode(error: unknown, code: number): boolean {
  for (let current = error; typeof current === "object" && current !== null; ) {
    const value = current as { code?: unknown; cause?: unknown; data?: { originalError?: unknown } };
    if (value.code === code) {
      return true;
    }
    const nested = value.data?.originalError;
    if (typeof nested === "object" && nested !== null && hasCode(nested, code)) {
      return true;
    }
    current = value.cause;
  }
  return false;
}
