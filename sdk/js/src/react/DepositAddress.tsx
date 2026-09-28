"use client";

import { useState } from "react";
import { networkName } from "../chains.js";
import { depositAddressTransfer, type DepositAddressDetails } from "../payment.js";
import { STYLES, appearanceStyle, type Appearance } from "./appearance.js";
import { Field } from "./Field.js";
import { QrCode } from "./QrCode.js";

export interface DepositAddressProps {
  /** `address` and `networks` from your backend's `POST /v1/deposit_addresses`. */
  depositAddress: DepositAddressDetails;
  /** The network shown first; the first of `networks` by default. */
  chainId?: number;
  /** The token shown first, for example `pha`; the network's first token by default. */
  asset?: string;
  appearance?: Appearance;
  className?: string;
}

/**
 * A customer's persistent deposit address: one address for every supported token on every
 * supported network. The payer picks a network and a token; the component shows a QR code of that
 * EIP-681 transfer request and the address and token contract to copy. Any amount of a supported
 * token sent is credited at the market rate when it arrives; follow it from your backend's
 * `deposit.credited` webhook.
 */
export function DepositAddress({
  depositAddress,
  chainId,
  asset,
  appearance,
  className,
}: DepositAddressProps) {
  const { networks } = depositAddress;
  const [selectedChain, setSelectedChain] = useState(chainId ?? networks[0]?.chain_id);
  const [selectedAsset, setSelectedAsset] = useState(asset);
  const network = networks.find((candidate) => candidate.chain_id === selectedChain) ?? networks[0];
  if (network === undefined) {
    throw new TypeError("the deposit address has no network");
  }
  const token =
    network.assets.find((candidate) => candidate.asset === selectedAsset) ?? network.assets[0];
  if (token === undefined) {
    throw new TypeError(`the deposit address takes no token on chain ${network.chain_id}`);
  }
  const { token: contract, to } = depositAddressTransfer(network, token);
  const symbol = token.asset.toUpperCase();
  const name = networkName(network.chain_id);
  const tokens = [...new Set(networks.flatMap((each) => each.assets.map((a) => a.asset.toUpperCase())))];
  return (
    <div
      className={className === undefined ? "pp-root" : `pp-root ${className}`}
      data-theme={appearance?.theme ?? "light"}
      style={appearanceStyle(appearance)}
    >
      <style>{STYLES}</style>
      <p className="pp-subtitle">
        {depositAddress.address === null
          ? "Your deposit address for every supported token; it differs on some networks"
          : "One address for all supported tokens and networks"}
      </p>
      {networks.length > 1 && (
        <div className="pp-tabs" role="tablist" aria-label="Network">
          {networks.map((each) => (
            <button
              key={each.chain_id}
              type="button"
              role="tab"
              className="pp-tab"
              aria-selected={each.chain_id === network.chain_id}
              onClick={() => {
                setSelectedChain(each.chain_id);
              }}
            >
              {networkName(each.chain_id)}
            </button>
          ))}
        </div>
      )}
      {network.assets.length > 1 && (
        <div className="pp-tabs" role="tablist" aria-label="Token">
          {network.assets.map((each) => (
            <button
              key={each.asset}
              type="button"
              role="tab"
              className="pp-tab"
              aria-selected={each.asset === token.asset}
              onClick={() => {
                setSelectedAsset(each.asset);
              }}
            >
              {each.asset.toUpperCase()}
            </button>
          ))}
        </div>
      )}
      <div className="pp-qr-panel">
        <QrCode value={token.payment_uri} label={`Deposit address for ${symbol} on ${name}`} />
      </div>
      <dl className="pp-fields">
        <Field label="Network" value={`${name} (chain ID ${network.chain_id})`} />
        <Field label={`Token (${symbol}) contract`} value={contract} copy />
        <Field label="Deposit address" value={to} copy />
      </dl>
      <p className="pp-message">
        Send only {tokens.join(", ")} on {networks.map((each) => networkName(each.chain_id)).join(", ")}
        . Any amount is credited at the market rate when it arrives, usually in about 30 seconds.
        Other tokens and networks are not credited. You can reuse this address.
      </p>
    </div>
  );
}
