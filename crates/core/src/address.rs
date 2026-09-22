//! Deterministic salt derivation and EIP-1167 CREATE2 address prediction.

use alloy_primitives::{Address, B256, U256, keccak256};
use alloy_sol_types::SolValue;

/// Derives the salt for a persistent deposit address.
#[must_use]
pub fn persistent_salt(product_slug: &str, external_id: &str, version: u64) -> B256 {
    keccak256(
        (product_slug, external_id, U256::from(version))
            .abi_encode_params()
            .as_slice(),
    )
}

/// Derives the salt for a single-use rate-lock deposit address.
#[must_use]
pub fn lock_salt(product_slug: &str, external_id: &str, lock_ref: &str) -> B256 {
    keccak256(
        (product_slug, external_id, "lock", lock_ref)
            .abi_encode_params()
            .as_slice(),
    )
}

/// Predicts the deterministic OpenZeppelin EIP-1167 clone address.
#[must_use]
pub fn forwarder_address(factory: Address, implementation: Address, salt: B256) -> Address {
    const CREATION_PREFIX: &[u8] = &alloy_primitives::hex!("3d602d80600a3d3981f3");
    const RUNTIME_PREFIX: &[u8] = &alloy_primitives::hex!("363d3d373d3d3d363d73");
    const RUNTIME_SUFFIX: &[u8] = &alloy_primitives::hex!("5af43d82803e903d91602b57fd5bf3");

    let mut init_code = Vec::with_capacity(55);
    init_code.extend_from_slice(CREATION_PREFIX);
    init_code.extend_from_slice(RUNTIME_PREFIX);
    init_code.extend_from_slice(implementation.as_slice());
    init_code.extend_from_slice(RUNTIME_SUFFIX);

    let init_code_hash = keccak256(init_code);
    let mut preimage = Vec::with_capacity(85);
    preimage.push(0xff);
    preimage.extend_from_slice(factory.as_slice());
    preimage.extend_from_slice(salt.as_slice());
    preimage.extend_from_slice(init_code_hash.as_slice());

    Address::from_word(keccak256(preimage))
}
