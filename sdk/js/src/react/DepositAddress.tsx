"use client";

import { networkName } from "../chains.js";
import { depositAddressTransfer, type DepositAddressDetails } from "../payment.js";
import { STYLES, appearanceStyle, type Appearance } from "./appearance.js";
import { Field } from "./Field.js";
import { QrCode } from "./QrCode.js";

export interface DepositAddressProps {
  /** `address`, `chain_id`, `asset`, and `payment_uri` from your backend's
   * `POST /v1/deposit_addresses`. */
  depositAddress: DepositAddressDetails;
  appearance?: Appearance;
  className?: string;
}

/**
 * A customer's persistent deposit address: its network, a QR code of the EIP-681 transfer
 * request, and the token contract and address to copy. Any amount sent is credited at the market
 * rate when it arrives; follow it from your backend's `deposit.credited` webhook.
 */
export function DepositAddress({ depositAddress, appearance, className }: DepositAddressProps) {
  const { token, to } = depositAddressTransfer(depositAddress);
  const asset = depositAddress.asset.toUpperCase();
  const network = networkName(depositAddress.chain_id);
  return (
    <div
      className={className === undefined ? "pp-root" : `pp-root ${className}`}
      data-theme={appearance?.theme ?? "light"}
      style={appearanceStyle(appearance)}
    >
      <style>{STYLES}</style>
      <p className="pp-subtitle">
        Send {asset} on {network}
      </p>
      <div className="pp-qr-panel">
        <QrCode value={depositAddress.payment_uri} label={`Deposit address for ${asset} on ${network}`} />
      </div>
      <dl className="pp-fields">
        <Field label="Network" value={`${network} (chain ID ${depositAddress.chain_id})`} />
        <Field label={`Token (${asset}) contract`} value={token} copy />
        <Field label="Deposit address" value={to} copy />
      </dl>
      <p className="pp-message">
        Send any amount of {asset} on {network} only. It is credited at the market rate when it
        arrives, usually in about 30 seconds. You can reuse this address.
      </p>
    </div>
  );
}
