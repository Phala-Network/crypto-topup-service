//! dstack attestation primitives.

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::num::NonZeroU32;
use std::time::Duration;

use alloy_primitives::Address;
use dstack_sdk::dstack_client::DstackClient;
use sha2::{Digest, Sha256};
use tokio::time::timeout;
use topup_core::{Ed25519PublicKey, SETTLEMENT_KEY_DOMAIN, Signer as _};
use zeroize::Zeroize as _;

use crate::signer::dstack::{DerivedKey, DstackSigner, KeyAlgorithm};
use crate::signer::settlement_public_key;

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// dstack identity metadata returned alongside an attestation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationInfo {
    /// Application identifier.
    pub app_id: Vec<u8>,
    /// Hash of the deployed compose document.
    pub compose_hash: Vec<u8>,
    /// Verbatim compose document, when the SDK exposes it.
    pub app_compose: Option<String>,
}

/// The operator key a chain's flusher signs with: `operator/v{key_version}`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct OperatorKey {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The chain's configured `operator_key_version`.
    pub key_version: NonZeroU32,
}

/// A flusher operator identity bound into the report data.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct AttestedOperator {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Derivation version of the operator key.
    pub key_version: NonZeroU32,
    /// Address of the derived secp256k1 operator key.
    pub address: Address,
}

impl AttestedOperator {
    /// Returns the 32-byte report-data record `chain_id (u64 BE) ‖ key_version (u32 BE) ‖ address`.
    #[must_use]
    pub fn record(&self) -> [u8; 32] {
        let mut record = [0_u8; 32];
        record[..8].copy_from_slice(&self.chain_id.to_be_bytes());
        record[8..12].copy_from_slice(&self.key_version.get().to_be_bytes());
        record[12..].copy_from_slice(self.address.as_slice());
        record
    }
}

/// An attestation binding a nonce to the settlement public key and the flusher operators.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationBundle {
    /// Settlement public key bound into the report data.
    pub settlement_public_key: Ed25519PublicKey,
    /// Flusher operators bound into the report data, in ascending chain order.
    pub operators: Vec<AttestedOperator>,
    /// SHA-256 commitment passed as dstack report data.
    pub report_data: [u8; 32],
    /// Versioned dstack attestation bytes. On TDX this contains the quote and event log.
    pub quote: Vec<u8>,
    /// Application identity and compose metadata.
    pub info: AttestationInfo,
}

/// A failure while collecting dstack attestation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationError {
    /// The settlement key returned by dstack was malformed.
    InvalidSettlementKey,
    /// An operator key could not be derived.
    OperatorKeyUnavailable,
    /// The dstack attestation or information call failed or timed out.
    DstackUnavailable,
}

impl Display for AttestationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSettlementKey => "dstack returned an invalid settlement key",
            Self::OperatorKeyUnavailable => "an operator key could not be derived",
            Self::DstackUnavailable => "dstack attestation is unavailable",
        };
        formatter.write_str(message)
    }
}

impl Error for AttestationError {}

/// Computes `sha256(nonce ‖ settlement_public_key ‖ record_1 ‖ … ‖ record_n)`.
///
/// Each operator contributes its fixed-width [`AttestedOperator::record`], in ascending
/// `(chain_id, key_version, address)` order whatever the order of `operators`. Without operators
/// this is the original `sha256(nonce ‖ settlement_public_key)`.
#[must_use]
pub fn report_data(
    nonce: &[u8],
    settlement_public_key: &Ed25519PublicKey,
    operators: &[AttestedOperator],
) -> [u8; 32] {
    let mut operators = operators.to_vec();
    operators.sort_unstable();
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(settlement_public_key.0);
    for operator in &operators {
        hasher.update(operator.record());
    }
    hasher.finalize().into()
}

/// Collects a settlement-key attestation from the dstack guest agent.
#[derive(Clone, Debug)]
pub struct DstackAttestor {
    endpoint: Option<String>,
    timeout: Duration,
}

impl Default for DstackAttestor {
    fn default() -> Self {
        Self::new()
    }
}

impl DstackAttestor {
    /// Uses the dstack socket selected by the SDK and a ten-second call timeout.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            endpoint: None,
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Uses the dstack socket selected by the SDK and the provided call timeout.
    #[must_use]
    pub const fn with_timeout(timeout: Duration) -> Self {
        Self {
            endpoint: None,
            timeout,
        }
    }

    /// Uses an explicit dstack or simulator endpoint and a ten-second call timeout.
    #[must_use]
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
            timeout: DEFAULT_TIMEOUT,
        }
    }

    /// Uses an explicit endpoint and timeout for every dstack SDK call.
    #[must_use]
    pub fn with_endpoint_and_timeout(endpoint: impl Into<String>, timeout: Duration) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
            timeout,
        }
    }

    /// Derives the settlement key and each operator key, binds them to `nonce`, and returns
    /// dstack evidence.
    ///
    /// Operator addresses come from the same `operator/v{n}` derivation the flusher signs with.
    pub async fn attest(
        &self,
        nonce: &[u8],
        operator_keys: &[OperatorKey],
    ) -> Result<AttestationBundle, AttestationError> {
        let mut operators = Vec::with_capacity(operator_keys.len());
        for key in operator_keys {
            let signer = match &self.endpoint {
                Some(endpoint) => DstackSigner::with_endpoint_and_timeout(endpoint, self.timeout),
                None => DstackSigner::with_timeout(self.timeout),
            };
            let address = signer
                .with_operator_key_version(key.key_version)
                .operator_address()
                .await
                .map_err(|_| AttestationError::OperatorKeyUnavailable)?;
            operators.push(AttestedOperator {
                chain_id: key.chain_id,
                key_version: key.key_version,
                address,
            });
        }
        operators.sort_unstable();
        operators.dedup();

        let client = DstackClient::new(self.endpoint.as_deref());
        let mut key_response = timeout(
            self.timeout,
            client.get_key(Some(SETTLEMENT_KEY_DOMAIN.to_owned()), None),
        )
        .await
        .map_err(|_| AttestationError::DstackUnavailable)?
        .map_err(|_| AttestationError::DstackUnavailable)?;
        let key = key_response.decode_key();
        key_response.key.zeroize();
        let key = key
            .map_err(|_| AttestationError::InvalidSettlementKey)
            .and_then(|key| {
                DerivedKey::from_bytes(key, KeyAlgorithm::Ed25519)
                    .map_err(|_| AttestationError::InvalidSettlementKey)
            })?;
        let settlement_public_key = settlement_public_key(&key.secret);
        drop(key);

        let report_data = report_data(nonce, &settlement_public_key, &operators);
        let response = timeout(self.timeout, client.attest(report_data.to_vec()))
            .await
            .map_err(|_| AttestationError::DstackUnavailable)?
            .map_err(|_| AttestationError::DstackUnavailable)?;
        let info = timeout(self.timeout, client.info())
            .await
            .map_err(|_| AttestationError::DstackUnavailable)?
            .map_err(|_| AttestationError::DstackUnavailable)?;
        let quote = response
            .decode_attestation()
            .map_err(|_| AttestationError::DstackUnavailable)?;
        let app_id = hex::decode(&info.app_id).map_err(|_| AttestationError::DstackUnavailable)?;
        let compose_hash =
            hex::decode(&info.compose_hash).map_err(|_| AttestationError::DstackUnavailable)?;
        let app_compose = info.tcb_info.app_compose;

        Ok(AttestationBundle {
            settlement_public_key,
            operators,
            report_data,
            quote,
            info: AttestationInfo {
                app_id,
                compose_hash,
                app_compose: (!app_compose.is_empty()).then_some(app_compose),
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use alloy_primitives::Address;
    use topup_core::Ed25519PublicKey;

    use super::{AttestedOperator, report_data};

    fn nonce() -> Vec<u8> {
        hex::decode("000102030405060708090a0b0c0d0e0f").expect("valid vector")
    }

    #[test]
    fn report_data_without_operators_matches_the_original_vector() {
        assert_eq!(
            hex::encode(report_data(&nonce(), &Ed25519PublicKey([0x42; 32]), &[])),
            "58c4e8b13ba082a25854a52564151194a7ec3221acc8aa8884f2aba2dda1037f"
        );
    }

    #[test]
    fn report_data_binds_operators_in_canonical_order() {
        let sepolia = AttestedOperator {
            chain_id: 11_155_111,
            key_version: NonZeroU32::MIN,
            address: Address::repeat_byte(0x22),
        };
        let mainnet = AttestedOperator {
            chain_id: 1,
            key_version: NonZeroU32::new(2).expect("two is non-zero"),
            address: Address::repeat_byte(0x11),
        };
        let expected = "c30486f4d5a70ddf9a44157ce18f8c1e37a9479f15c02b80c4e373dc639fa9ea";
        let key = Ed25519PublicKey([0x42; 32]);

        assert_eq!(
            hex::encode(mainnet.record()),
            concat!(
                "0000000000000001",
                "00000002",
                "1111111111111111111111111111111111111111"
            )
        );
        assert_eq!(
            hex::encode(report_data(&nonce(), &key, &[mainnet, sepolia])),
            expected
        );
        assert_eq!(
            hex::encode(report_data(&nonce(), &key, &[sepolia, mainnet])),
            expected
        );
    }
}
