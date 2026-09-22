//! Deterministic identities for observed chain events.

use alloy_primitives::B256;
use uuid::Uuid;

/// Namespace for crypto top-up deposit UUIDs.
///
/// This is UUIDv5(URL, `https://github.com/Phala-Network/crypto-topup-service/deposit`).
/// Keeping the derived value explicit makes implementations in other languages independent of
/// repository or DNS lookups.
pub const DEPOSIT_NAMESPACE: Uuid = Uuid::from_u128(0xd55bab89f6565796a6a2bddfa1dd9631);

/// Returns the UUIDv5 identity for an EVM transfer log.
///
/// The name is `{chain_id}:{tx_hash lowercase 0x-prefixed}:{log_index decimal}`.
#[must_use]
pub fn deposit_id(chain_id: u64, tx_hash: B256, log_index: u64) -> Uuid {
    let name = format!("{chain_id}:{tx_hash:#x}:{log_index}");
    Uuid::new_v5(&DEPOSIT_NAMESPACE, name.as_bytes())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::b256;

    use super::*;

    #[test]
    fn matches_python_uuid5_vector() {
        let tx_hash = b256!("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef");

        // Python 3: uuid.uuid5(UUID("d55bab89-f656-5796-a6a2-bddfa1dd9631"), name)
        assert_eq!(
            deposit_id(1, tx_hash, 42).to_string(),
            "20513a59-9b80-53df-9832-08749de3dcc5"
        );
    }
}
