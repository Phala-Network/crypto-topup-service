//! ABI boundary for forwarder-factory flush operations.

use alloy_primitives::{Address, B256, Bytes, LogData, U256};
use alloy_sol_types::{SolCall, SolEvent, sol};

sol! {
    function balanceOf(address account) external view returns (uint256);
    function addressOf(address treasury, bytes32 salt) external view returns (address);
    function flush(address treasury, bytes32[] salts, address token) external;
    function implementation() external view returns (address);
    function factory() external view returns (address);
    event Flushed(
        bytes32 indexed salt,
        address indexed forwarder,
        address indexed token,
        address treasury,
        uint256 amount
    );
    event FlushFailed(
        bytes32 indexed salt,
        address indexed forwarder,
        address indexed token,
        bytes reason
    );
}

/// A decoded `ForwarderFactory.Flushed` event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DecodedFlushed {
    /// CREATE2 salt identifying the forwarder.
    pub salt: B256,
    /// Forwarder address emptied by the factory.
    pub forwarder: Address,
    /// Token transferred to the treasury.
    pub token: Address,
    /// Treasury the forwarder paid, its only immutable argument.
    pub treasury: Address,
    /// Amount that left the forwarder.
    pub amount: U256,
}

/// A decoded `ForwarderFactory.FlushFailed` event: one target of a batch whose transfer failed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedFlushFailed {
    /// CREATE2 salt identifying the forwarder.
    pub salt: B256,
    /// Forwarder address whose flush failed.
    pub forwarder: Address,
    /// Token that could not be transferred.
    pub token: Address,
    /// Revert data, truncated by the factory to 256 bytes.
    pub reason: Bytes,
}

/// Encodes an ERC-20 `balanceOf` call.
#[must_use]
pub fn encode_balance_of(account: Address) -> Bytes {
    balanceOfCall { account }.abi_encode().into()
}

/// Encodes `ForwarderFactory.addressOf(treasury, salt)`.
#[must_use]
pub fn encode_address_of(treasury: Address, salt: B256) -> Bytes {
    addressOfCall { treasury, salt }.abi_encode().into()
}

/// Encodes a view call to `ForwarderFactory.implementation()` or `Forwarder.factory()`.
#[must_use]
pub fn encode_contract_address_getter(getter: ContractAddressGetter) -> Bytes {
    match getter {
        ContractAddressGetter::Implementation => implementationCall {}.abi_encode().into(),
        ContractAddressGetter::Factory => factoryCall {}.abi_encode().into(),
    }
}

/// Decodes the address returned by one of the forwarder contracts' immutable getters.
pub fn decode_contract_address_getter(
    getter: ContractAddressGetter,
    output: &[u8],
) -> Result<Address, alloy_sol_types::Error> {
    match getter {
        ContractAddressGetter::Implementation => implementationCall::abi_decode_returns(output),
        ContractAddressGetter::Factory => factoryCall::abi_decode_returns(output),
    }
}

/// Immutable address getters of the forwarder contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractAddressGetter {
    /// `ForwarderFactory.implementation()`.
    Implementation,
    /// `Forwarder.factory()`.
    Factory,
}

/// Encodes `ForwarderFactory.flush(treasury, salts, token)`.
#[must_use]
pub fn encode_flush(treasury: Address, salts: Vec<B256>, token: Address) -> Bytes {
    flushCall {
        treasury,
        salts,
        token,
    }
    .abi_encode()
    .into()
}

/// Decodes a `ForwarderFactory.Flushed` log.
pub fn decode_flushed(log: &LogData) -> Result<DecodedFlushed, alloy_sol_types::Error> {
    Flushed::decode_log_data_validate(log).map(|event| DecodedFlushed {
        salt: event.salt,
        forwarder: event.forwarder,
        token: event.token,
        treasury: event.treasury,
        amount: event.amount,
    })
}

/// Returns the first topic of `ForwarderFactory.Flushed`.
#[must_use]
pub const fn flushed_signature() -> B256 {
    Flushed::SIGNATURE_HASH
}

/// Decodes a `ForwarderFactory.FlushFailed` log.
pub fn decode_flush_failed(log: &LogData) -> Result<DecodedFlushFailed, alloy_sol_types::Error> {
    FlushFailed::decode_log_data_validate(log).map(|event| DecodedFlushFailed {
        salt: event.salt,
        forwarder: event.forwarder,
        token: event.token,
        reason: event.reason,
    })
}

/// Returns the first topic of `ForwarderFactory.FlushFailed`.
#[must_use]
pub const fn flush_failed_signature() -> B256 {
    FlushFailed::SIGNATURE_HASH
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{LogData, address, b256};
    use alloy_sol_types::SolEvent;

    use super::*;

    #[test]
    fn round_trips_factory_calls_and_event() {
        let salt = b256!("1111111111111111111111111111111111111111111111111111111111111111");
        let token = address!("2222222222222222222222222222222222222222");
        let forwarder = address!("3333333333333333333333333333333333333333");
        let treasury = address!("4444444444444444444444444444444444444444");
        let call = encode_flush(treasury, vec![salt], token);
        let decoded = flushCall::abi_decode(&call).expect("flush calldata should decode");
        assert_eq!(decoded.treasury, treasury);
        assert_eq!(decoded.salts, vec![salt]);
        assert_eq!(decoded.token, token);

        let event = Flushed {
            salt,
            forwarder,
            token,
            treasury,
            amount: U256::from(42_u8),
        };
        let encoded = event.encode_log_data();
        let log = LogData::new(encoded.topics().to_vec(), encoded.data.clone())
            .expect("generated event topics are valid");
        assert_eq!(
            decode_flushed(&log).expect("event should decode"),
            DecodedFlushed {
                salt,
                forwarder,
                token,
                treasury,
                amount: U256::from(42_u8),
            }
        );

        let failed = FlushFailed {
            salt,
            forwarder,
            token,
            reason: Bytes::from_static(&[0xde, 0xad]),
        }
        .encode_log_data();
        let log = LogData::new(failed.topics().to_vec(), failed.data.clone())
            .expect("generated event topics are valid");
        assert_eq!(
            decode_flush_failed(&log).expect("event should decode"),
            DecodedFlushFailed {
                salt,
                forwarder,
                token,
                reason: Bytes::from_static(&[0xde, 0xad]),
            }
        );
    }
}
