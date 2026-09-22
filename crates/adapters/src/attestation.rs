//! dstack attestation primitives.

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use sha2::{Digest, Sha256};
use topup_core::{Ed25519PublicKey, SETTLEMENT_KEY_DOMAIN};

use crate::signer::dstack::{DerivedKey, run_dstack};

const ED25519_ALGORITHM: &str = "ed25519";

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
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationError {
    /// The settlement key returned by dstack was malformed.
    InvalidSettlementKey,
    /// The dstack attestation or information call failed.
    DstackUnavailable,
}

impl Display for AttestationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidSettlementKey => "dstack returned an invalid settlement key",
            Self::DstackUnavailable => "dstack attestation is unavailable",
        };
        formatter.write_str(message)
    }
}

impl Error for AttestationError {}

/// Computes `sha256(nonce || settlement_public_key)`.
#[must_use]
pub fn report_data(nonce: &[u8], settlement_public_key: &Ed25519PublicKey) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(nonce);
    hasher.update(settlement_public_key.0);
    hasher.finalize().into()
}

/// Collects a settlement-key attestation from dstack v1.
#[derive(Clone, Debug, Default)]
pub struct DstackAttestor {
    endpoint: Option<String>,
}

impl DstackAttestor {
    /// Uses the dstack socket selected by the SDK.
    #[must_use]
    pub const fn new() -> Self {
        Self { endpoint: None }
    }

    /// Uses an explicit dstack or simulator endpoint.
    #[must_use]
    pub fn with_endpoint(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: Some(endpoint.into()),
        }
    }

    /// Derives the settlement key, binds it to `nonce`, and returns dstack evidence.
    pub fn attest(&self, nonce: &[u8]) -> Result<AttestationBundle, AttestationError> {
        let nonce = nonce.to_vec();
        run_dstack(
            self.endpoint.clone(),
            AttestationError::DstackUnavailable,
            move |client| async move {
                let key_response = client
                    .get_key(SETTLEMENT_KEY_DOMAIN, ED25519_ALGORITHM)
                    .await
                    .map_err(|_| AttestationError::DstackUnavailable)?;
                let key = DerivedKey::from_response(key_response.key, key_response.public_key)
                    .map_err(|_| AttestationError::InvalidSettlementKey)?;
                let settlement_public_key = super::signer::settlement_public_key(&key.secret);
                if settlement_public_key.0 != key.public_key.as_slice() {
                    return Err(AttestationError::InvalidSettlementKey);
                }
                let report_data = report_data(&nonce, &settlement_public_key);
                let response = client
                    .attest(report_data.to_vec(), false)
                    .await
                    .map_err(|_| AttestationError::DstackUnavailable)?;
                let info = client
                    .info()
                    .await
                    .map_err(|_| AttestationError::DstackUnavailable)?;
                drop(key);

                Ok(AttestationBundle {
                    settlement_public_key,
                    report_data,
                    quote: response.attestation,
                    info: AttestationInfo {
                        app_id: info.app_id,
                        compose_hash: info.compose_hash,
                        app_compose: (!info.app_compose.is_empty()).then_some(info.app_compose),
                    },
                })
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use topup_core::Ed25519PublicKey;

    use super::report_data;

    #[test]
    fn report_data_matches_known_vector() {
        let nonce = hex::decode("000102030405060708090a0b0c0d0e0f").expect("valid vector");
        let public_key = Ed25519PublicKey([0x42; 32]);

        assert_eq!(
            hex::encode(report_data(&nonce, &public_key)),
            "58c4e8b13ba082a25854a52564151194a7ec3221acc8aa8884f2aba2dda1037f"
        );
    }
}
