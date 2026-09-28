//! dstack attestation primitives.

use std::time::Duration;

use dstack_sdk::dstack_client::DstackClient;
use sha2::{Digest, Sha256};
use tokio::time::timeout;
use topup_core::{Ed25519PublicKey, SETTLEMENT_KEY_DOMAIN};
use zeroize::Zeroize as _;

use crate::signer::dstack::{DerivedKey, KeyAlgorithm};
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

/// An attestation binding a nonce to the settlement public key.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AttestationBundle {
    /// Settlement public key bound into the report data.
    pub settlement_public_key: Ed25519PublicKey,
    /// SHA-256 commitment passed as dstack report data.
    pub report_data: [u8; 32],
    /// Versioned dstack attestation bytes. On TDX this contains the quote and event log.
    pub quote: Vec<u8>,
    /// Application identity and compose metadata.
    pub info: AttestationInfo,
}

/// A failure while collecting dstack attestation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AttestationError {
    /// The settlement key returned by dstack was malformed.
    #[error("dstack returned an invalid settlement key")]
    InvalidSettlementKey,
    /// The dstack attestation or information call failed or timed out.
    #[error("dstack attestation is unavailable")]
    DstackUnavailable,
}

/// Computes `sha256(nonce ‖ settlement_public_key)`.
#[must_use]
pub fn report_data(nonce: &[u8], settlement_public_key: &Ed25519PublicKey) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(settlement_public_key.0);
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

    /// Derives the settlement key, binds it to `nonce`, and returns dstack evidence.
    pub async fn attest(&self, nonce: &[u8]) -> Result<AttestationBundle, AttestationError> {
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

        let report_data = report_data(nonce, &settlement_public_key);
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
    use topup_core::Ed25519PublicKey;

    use super::report_data;

    #[test]
    fn report_data_matches_the_published_vector() {
        let nonce = hex::decode("000102030405060708090a0b0c0d0e0f").expect("valid vector");
        assert_eq!(
            hex::encode(report_data(&nonce, &Ed25519PublicKey([0x42; 32]))),
            "58c4e8b13ba082a25854a52564151194a7ec3221acc8aa8884f2aba2dda1037f"
        );
    }
}
