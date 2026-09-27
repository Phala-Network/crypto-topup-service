//! Deterministic salt derivation and CREATE2 address prediction for forwarder clones.

use alloy_primitives::{Address, B256, keccak256};
use alloy_sol_types::SolValue;

/// Derives the salt for a quote's single-use deposit address; a quote's `lock_ref` is its id.
#[must_use]
pub fn lock_salt(product_slug: &str, external_id: &str, lock_ref: &str) -> B256 {
    keccak256(
        (product_slug, external_id, "lock", lock_ref)
            .abi_encode_params()
            .as_slice(),
    )
}

/// Predicts the address of the forwarder clone that `factory` deploys for `treasury` and `salt`.
///
/// This is OpenZeppelin 5.x `Clones.predictDeterministicAddressWithImmutableArgs` with the clone
/// arguments `abi.encodePacked(treasury)`: the init code is the EIP-1167 proxy for
/// `implementation` preceded by a creation header and followed by the 20 treasury bytes, so the
/// address commits to the factory, the implementation, the treasury, and the salt.
#[must_use]
pub fn forwarder_address(
    factory: Address,
    implementation: Address,
    treasury: Address,
    salt: B256,
) -> Address {
    // `PUSH2 <runtime length>` then `RETURNDATASIZE DUP2 PUSH1 0x0a RETURNDATASIZE CODECOPY
    // RETURN`; the runtime is the 45-byte proxy followed by the arguments.
    const RUNTIME_LENGTH: [u8; 2] = [0x00, 0x2d + 20];
    const CREATION_SUFFIX: &[u8] = &alloy_primitives::hex!("3d81600a3d39f3");
    const RUNTIME_PREFIX: &[u8] = &alloy_primitives::hex!("363d3d373d3d3d363d73");
    const RUNTIME_SUFFIX: &[u8] = &alloy_primitives::hex!("5af43d82803e903d91602b57fd5bf3");

    let mut init_code = Vec::with_capacity(75);
    init_code.push(0x61);
    init_code.extend_from_slice(&RUNTIME_LENGTH);
    init_code.extend_from_slice(CREATION_SUFFIX);
    init_code.extend_from_slice(RUNTIME_PREFIX);
    init_code.extend_from_slice(implementation.as_slice());
    init_code.extend_from_slice(RUNTIME_SUFFIX);
    init_code.extend_from_slice(treasury.as_slice());

    factory.create2(salt, keccak256(init_code))
}
