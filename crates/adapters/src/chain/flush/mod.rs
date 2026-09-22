//! ABI boundary for forwarder-factory flush operations.

use alloy_primitives::{Address, B256, Bytes, LogData, U256};
use alloy_sol_types::{SolCall, SolEvent, sol};

sol! {
    function balanceOf(address account) external view returns (uint256);
    function addressOf(bytes32 salt) external view returns (address);
    function flush(bytes32[] salts, address token) external;
    event Flushed(
        bytes32 indexed salt,
        address indexed forwarder,
        address indexed token,
        uint256 amount
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
    /// Amount received by the treasury.
    pub amount: U256,
}

/// Encodes an ERC-20 `balanceOf` call.
#[must_use]
pub fn encode_balance_of(account: Address) -> Bytes {
    balanceOfCall { account }.abi_encode().into()
}

/// Decodes an ERC-20 `balanceOf` result.
pub fn decode_balance_of(output: &[u8]) -> Result<U256, alloy_sol_types::Error> {
    balanceOfCall::abi_decode_returns(output)
}

/// Encodes `ForwarderFactory.addressOf(salt)`.
#[must_use]
pub fn encode_address_of(salt: B256) -> Bytes {
    addressOfCall { salt }.abi_encode().into()
}

/// Decodes a `ForwarderFactory.addressOf(salt)` result.
pub fn decode_address_of(output: &[u8]) -> Result<Address, alloy_sol_types::Error> {
    addressOfCall::abi_decode_returns(output)
}

/// Encodes `ForwarderFactory.flush(salts, token)`.
#[must_use]
pub fn encode_flush(salts: Vec<B256>, token: Address) -> Bytes {
    flushCall { salts, token }.abi_encode().into()
}

/// Decodes a `ForwarderFactory.Flushed` log.
pub fn decode_flushed(log: &LogData) -> Result<DecodedFlushed, alloy_sol_types::Error> {
    Flushed::decode_log_data_validate(log).map(|event| DecodedFlushed {
        salt: event.salt,
        forwarder: event.forwarder,
        token: event.token,
        amount: event.amount,
    })
}

/// Returns the first topic of `ForwarderFactory.Flushed`.
#[must_use]
pub const fn flushed_signature() -> B256 {
    Flushed::SIGNATURE_HASH
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
        let call = encode_flush(vec![salt], token);
        let decoded = flushCall::abi_decode(&call).expect("flush calldata should decode");
        assert_eq!(decoded.salts, vec![salt]);
        assert_eq!(decoded.token, token);

        let event = Flushed {
            salt,
            forwarder,
            token,
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
                amount: U256::from(42_u8),
            }
        );
    }
}
