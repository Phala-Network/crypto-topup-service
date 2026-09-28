import {
  encodeAbiParameters,
  getAddress,
  getContractAddress,
  keccak256,
  concat,
  type Address,
  type Hex,
} from "viem";

/**
 * The forwarder `factory` deploys for `treasury` and `salt`: OpenZeppelin 5.x
 * `Clones.predictDeterministicAddressWithImmutableArgs` with `abi.encodePacked(treasury)`, so the
 * address commits to the factory, the implementation, the treasury, and the salt.
 */
export function forwarderAddress(
  factory: string,
  implementation: string,
  treasury: string,
  salt: Hex,
): Address {
  const initCode = concat([
    "0x61",
    "0x0041", // runtime length: the 45-byte proxy and the 20 treasury bytes
    "0x3d81600a3d39f3",
    "0x363d3d373d3d3d363d73",
    getAddress(implementation),
    "0x5af43d82803e903d91602b57fd5bf3",
    getAddress(treasury),
  ]);
  return getContractAddress({
    opcode: "CREATE2",
    from: getAddress(factory),
    salt,
    bytecodeHash: keccak256(initCode),
  });
}

/** `keccak256(abi.encode(account, client_reference_id, "quote", quote_id))`: a quote's salt. */
export function quoteSalt(account: string, clientReferenceId: string, quoteId: string): Hex {
  return keccak256(
    encodeAbiParameters(
      [{ type: "string" }, { type: "string" }, { type: "string" }, { type: "string" }],
      [account, clientReferenceId, "quote", quoteId],
    ),
  );
}

/**
 * `keccak256(abi.encode(account, livemode, client_reference_id, "deposit_address", version))`:
 * a deposit address's salt, the same on every chain.
 */
export function depositAddressSalt(
  account: string,
  livemode: boolean,
  clientReferenceId: string,
  version: number | bigint,
): Hex {
  return keccak256(
    encodeAbiParameters(
      [
        { type: "string" },
        { type: "bool" },
        { type: "string" },
        { type: "string" },
        { type: "uint256" },
      ],
      [account, livemode, clientReferenceId, "deposit_address", BigInt(version)],
    ),
  );
}

export interface Forwarder {
  factory: string;
  implementation: string;
}

/** Recomputes a quote's address from the pinned forwarder and the quote's `treasury`. */
export function quoteAddress(
  forwarder: Forwarder,
  quote: { treasury: string; client_reference_id: string; id: string },
  account: string,
): Address {
  return forwarderAddress(
    forwarder.factory,
    forwarder.implementation,
    quote.treasury,
    quoteSalt(account, quote.client_reference_id, quote.id),
  );
}

/** Recomputes a deposit address on the network whose treasury is `treasury`. */
export function depositAddress(
  forwarder: Forwarder,
  address: { livemode: boolean; client_reference_id: string; version: number },
  treasury: string,
  account: string,
): Address {
  return forwarderAddress(
    forwarder.factory,
    forwarder.implementation,
    treasury,
    depositAddressSalt(account, address.livemode, address.client_reference_id, address.version),
  );
}
