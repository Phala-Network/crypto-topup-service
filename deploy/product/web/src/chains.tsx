import { cn } from "@/lib/utils";
import type { Asset, Network } from "./api.js";
import baseIcon from "./icons/base.svg";
import ethereumIcon from "./icons/ethereum.svg";
import phaIcon from "./icons/pha.svg";
import usdcIcon from "./icons/usdc.svg";

// Chain and token marks (./icons, from web3icons), bundled with the page: its CSP allows images
// from its own origin only.
const CHAIN_ICONS: Record<number, string> = {
  1: ethereumIcon,
  11155111: ethereumIcon,
  8453: baseIcon,
  84532: baseIcon,
};
const TOKENS: Record<string, { name: string; icon: string }> = {
  pha: { name: "Phala Network", icon: phaIcon },
  usdc: { name: "USD Coin", icon: usdcIcon },
};

/** The token's full name, `USD Coin`, or its symbol. */
export function tokenFullName(asset: string): string {
  return TOKENS[asset.toLowerCase()]?.name ?? asset.toUpperCase();
}

/** A chain's mark: a rounded square, as wallets show networks. */
export function ChainIcon({ chainId, className }: { chainId: number; className?: string }) {
  const icon = CHAIN_ICONS[chainId];
  return icon === undefined ? (
    <span className={cn("size-5 shrink-0 rounded-md bg-muted", className)} aria-hidden="true" />
  ) : (
    <img src={icon} alt="" className={cn("size-5 shrink-0 rounded-md", className)} />
  );
}

/** A token's mark: a circle, as wallets and exchanges show tokens. */
export function TokenIcon({ asset, className }: { asset: string; className?: string }) {
  const icon = TOKENS[asset.toLowerCase()]?.icon;
  return icon === undefined ? (
    <span className={cn("size-8 shrink-0 rounded-full bg-muted", className)} aria-hidden="true" />
  ) : (
    <img src={icon} alt="" className={cn("size-8 shrink-0 rounded-full", className)} />
  );
}

/** The network with `chainId`, else the first: a payment from before rows named their chain. */
export function networkOf(networks: Network[] | undefined, chainId: number | undefined): Network | undefined {
  return networks?.find((network) => network.chain_id === chainId) ?? networks?.[0];
}

/** The token `asset` on the network, for its symbol and decimals. */
export function assetOf(network: Network | undefined, asset: string | null | undefined): Asset | undefined {
  return network?.assets.find((each) => each.asset === asset?.toLowerCase());
}
