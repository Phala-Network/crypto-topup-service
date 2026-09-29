//! Deterministic identities for observed chain events.

use alloy_primitives::B256;
use uuid::Uuid;

/// Namespace for crypto top-up deposit UUIDs.
///
/// This is UUIDv5(URL, `https://github.com/Phala-Network/crypto-topup-service/deposit`).
/// Keeping the derived value explicit makes implementations in other languages independent of
/// repository or DNS lookups.
pub const DEPOSIT_NAMESPACE: Uuid = Uuid::from_u128(0xd55bab89f6565796a6a2bddfa1dd9631);

/// Returns the UUIDv5 identity of the first deposit recorded at an EVM transfer log's position.
///
/// The name is `{chain_id}:{tx_hash lowercase 0x-prefixed}:{receipt_log_index decimal}`, where
/// `receipt_log_index` is the log's position among the logs of its transaction's receipt (0 for
/// the first). Unlike the block-wide log index, the position survives the transaction's
/// re-inclusion in another block, so a transfer re-included unchanged keeps its deposit id.
///
/// The position does not fix the content: a re-included transaction that runs against other
/// state (a router, a swap) can put a different transfer at the same position. The deposit of the
/// old transfer is then reversed at finality, and the transfer now at the position is a new
/// deposit, [`deposit_revision_id`] with the next revision.
#[must_use]
pub fn deposit_id(chain_id: u64, tx_hash: B256, receipt_log_index: u64) -> Uuid {
    deposit_revision_id(chain_id, tx_hash, receipt_log_index, 0)
}

/// Returns the identity of the deposit recorded at a transfer log's position after `revision`
/// earlier deposits there were reversed: [`deposit_id`] for revision 0, and otherwise UUIDv5 over
/// `{chain_id}:{tx_hash}:{receipt_log_index}:{revision decimal}`.
#[must_use]
pub fn deposit_revision_id(
    chain_id: u64,
    tx_hash: B256,
    receipt_log_index: u64,
    revision: u64,
) -> Uuid {
    let name = if revision == 0 {
        format!("{chain_id}:{tx_hash:#x}:{receipt_log_index}")
    } else {
        format!("{chain_id}:{tx_hash:#x}:{receipt_log_index}:{revision}")
    };
    Uuid::new_v5(&DEPOSIT_NAMESPACE, name.as_bytes())
}

/// Returns the id of event `event_type` about the object with UUID `object_id`: UUIDv5 in the
/// deposit namespace over `{event_type}:{object_id}`, so every retry, replay, and re-emission
/// after a restore carries the same `webhook-id`.
///
/// The object is the deposit for `deposit.credited`, `deposit.rejected`, and `deposit.reversed`,
/// the refund for
/// `deposit.refunded`, and the quote for `quote.expired`.
#[must_use]
pub fn event_id(event_type: &str, object_id: Uuid) -> Uuid {
    let name = format!("{event_type}:{object_id}");
    Uuid::new_v5(&DEPOSIT_NAMESPACE, name.as_bytes())
}

/// Returns the event id of a deposit's `deposit.credited` webhook.
#[must_use]
pub fn credited_event_id(deposit_id: Uuid) -> Uuid {
    event_id("deposit.credited", deposit_id)
}

/// Returns the event id of a deposit's `deposit.reversed` webhook.
#[must_use]
pub fn reversed_event_id(deposit_id: Uuid) -> Uuid {
    event_id("deposit.reversed", deposit_id)
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
        assert_eq!(
            deposit_revision_id(1, tx_hash, 42, 0),
            deposit_id(1, tx_hash, 42)
        );
        assert_eq!(
            deposit_revision_id(1, tx_hash, 42, 1).to_string(),
            "718ca3c0-9ae4-55c7-b716-aa9dd168616f"
        );
    }

    #[test]
    fn credited_event_id_matches_python_sdk_vector() {
        // topup_sdk.credited_event_id (sdk/python/tests/test_fulfillment.py).
        let deposit = Uuid::from_u128(0x3f1c2b9e_6a8d_5c47_9e21_0b7d4f6a8c13);
        assert_eq!(
            credited_event_id(deposit).to_string(),
            "26a20351-ab10-595a-852f-9c1aa0372d73"
        );
    }
}
