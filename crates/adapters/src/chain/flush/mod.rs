//! ABI boundary for forwarder-factory flush operations.

use alloy_primitives::{Address, B256, Bytes, LogData, U256, keccak256};
use alloy_sol_types::{SolCall, SolEvent, sol};

sol! {
    function balanceOf(address account) external view returns (uint256);
    function addressOf(bytes32 salt) external view returns (address);
    function flush(bytes32[] salts, address token) external;
    function hasRole(bytes32 role, address account) external view returns (bool);
    function implementation() external view returns (address);
    function treasury() external view returns (address);
    function factory() external view returns (address);
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

/// Encodes `ForwarderFactory.addressOf(salt)`.
#[must_use]
pub fn encode_address_of(salt: B256) -> Bytes {
    addressOfCall { salt }.abi_encode().into()
}

/// Encodes a view call to `ForwarderFactory.implementation()`, `Forwarder.treasury()`, or
/// `Forwarder.factory()`.
#[must_use]
pub fn encode_contract_address_getter(getter: ContractAddressGetter) -> Bytes {
    match getter {
        ContractAddressGetter::Implementation => implementationCall {}.abi_encode().into(),
        ContractAddressGetter::Treasury => treasuryCall {}.abi_encode().into(),
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
        ContractAddressGetter::Treasury => treasuryCall::abi_decode_returns(output),
        ContractAddressGetter::Factory => factoryCall::abi_decode_returns(output),
    }
}

/// Immutable address getters of the forwarder contracts.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContractAddressGetter {
    /// `ForwarderFactory.implementation()`.
    Implementation,
    /// `Forwarder.treasury()`.
    Treasury,
    /// `Forwarder.factory()`.
    Factory,
}

/// Encodes `ForwarderFactory.flush(salts, token)`.
#[must_use]
pub fn encode_flush(salts: Vec<B256>, token: Address) -> Bytes {
    flushCall { salts, token }.abi_encode().into()
}

/// Returns `ForwarderFactory.OPERATOR_ROLE`, the role required to call `flush`.
#[must_use]
pub fn operator_role() -> B256 {
    keccak256("OPERATOR_ROLE")
}

/// Encodes `ForwarderFactory.hasRole(role, account)`.
#[must_use]
pub fn encode_has_role(role: B256, account: Address) -> Bytes {
    hasRoleCall { role, account }.abi_encode().into()
}

/// Decodes a `ForwarderFactory.hasRole(role, account)` result.
pub fn decode_has_role(output: &[u8]) -> Result<bool, alloy_sol_types::Error> {
    hasRoleCall::abi_decode_returns(output)
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
