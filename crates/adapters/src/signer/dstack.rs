//! dstack-backed signing adapter.
//!
//! The pinned SDK deserializes RPC JSON into ordinary response buffers before returning them. Those
//! internal JSON and response allocations are outside this adapter's zeroization control. Once the
//! private-key `Vec` reaches this module, it is immediately moved into [`Zeroizing`] and copied
//! directly into [`SecretKey32`]; both owned buffers are zeroized on drop.

use std::time::Duration;

use dstack_sdk::DstackClient;
use tokio::time::timeout;
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, OPERATOR_KEY_DOMAIN, SETTLEMENT_KEY_DOMAIN, SecretKey32,
    SignedTx, Signer, SignerError, TxRequest,
};
use zeroize::Zeroizing;

use super::{
    operator_address, operator_signer, settlement_public_key, sign_operator_tx, sign_settlement,
};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(10);

/// A signer which derives a fresh key for every operation through dstack v1.
#[derive(Clone, Debug)]
pub struct DstackSigner {
    endpoint: Option<String>,
    timeout: Duration,
}

impl Default for DstackSigner {
    fn default() -> Self {
        Self::new()
    }
}

impl DstackSigner {
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

    /// Derives the `backup/v1` secp256k1 key for backup encryption.
    ///
    /// The algorithm string is part of dstack's derivation input and is intentionally fixed here
    /// rather than exposed to callers.
    pub async fn derive_backup_key(&self) -> Result<SecretKey32, SignerError> {
        self.derive_backup_key_version(1).await
    }

    /// Derives a versioned `backup/vN` secp256k1 key for backup restore or rotation.
    pub async fn derive_backup_key_version(
        &self,
        version: u32,
    ) -> Result<SecretKey32, SignerError> {
        let domain = format!("backup/v{version}");
        Ok(self
            .derive_key(&domain, KeyAlgorithm::Secp256k1)
            .await?
            .secret)
    }

    async fn derive_key(
        &self,
        domain: &str,
        algorithm: KeyAlgorithm,
    ) -> Result<DerivedKey, SignerError> {
        let client = DstackClient::new(self.endpoint.as_deref());
        let response = timeout(self.timeout, client.get_key(domain, algorithm.as_str()))
            .await
            .map_err(|_| SignerError::KeyUnavailable)?
            .map_err(|_| SignerError::KeyUnavailable)?;
        DerivedKey::from_response(response.key, response.public_key, algorithm)
    }
}

impl Signer for DstackSigner {
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError> {
        let key = self
            .derive_key(OPERATOR_KEY_DOMAIN, KeyAlgorithm::Secp256k1)
            .await?;
        sign_operator_tx(&key.secret, tx)
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        let key = self
            .derive_key(SETTLEMENT_KEY_DOMAIN, KeyAlgorithm::Ed25519)
            .await?;
        Ok(sign_settlement(&key.secret, payload))
    }

    async fn operator_address(&self) -> Result<alloy_primitives::Address, SignerError> {
        let key = self
            .derive_key(OPERATOR_KEY_DOMAIN, KeyAlgorithm::Secp256k1)
            .await?;
        operator_address(&key.secret)
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        let key = self
            .derive_key(SETTLEMENT_KEY_DOMAIN, KeyAlgorithm::Ed25519)
            .await?;
        Ok(settlement_public_key(&key.secret))
    }
}

#[derive(Clone, Copy)]
pub(crate) enum KeyAlgorithm {
    Secp256k1,
    Ed25519,
}

impl KeyAlgorithm {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Secp256k1 => "secp256k1",
            Self::Ed25519 => "ed25519",
        }
    }
}

pub(crate) struct DerivedKey {
    pub(crate) secret: SecretKey32,
}

impl DerivedKey {
    pub(crate) fn from_response(
        key: Vec<u8>,
        public_key: Vec<u8>,
        algorithm: KeyAlgorithm,
    ) -> Result<Self, SignerError> {
        let key = Zeroizing::new(key);
        let secret = SecretKey32::from_slice(key.as_slice()).ok_or(SignerError::InvalidKey)?;
        validate_public_key(&secret, &public_key, algorithm)?;
        Ok(Self { secret })
    }
}

fn validate_public_key(
    secret: &SecretKey32,
    reported: &[u8],
    algorithm: KeyAlgorithm,
) -> Result<(), SignerError> {
    let matches = match algorithm {
        KeyAlgorithm::Secp256k1 => {
            let signer = operator_signer(secret)?;
            signer
                .credential()
                .verifying_key()
                .to_encoded_point(true)
                .as_bytes()
                == reported
        }
        KeyAlgorithm::Ed25519 => settlement_public_key(secret).0.as_slice() == reported,
    };
    if matches {
        Ok(())
    } else {
        Err(SignerError::InvalidKey)
    }
}

#[cfg(test)]
mod tests {
    use super::{DerivedKey, KeyAlgorithm};
    use topup_core::SignerError;

    #[test]
    fn rejects_wrong_private_key_length() {
        assert_eq!(
            DerivedKey::from_response(vec![1; 31], vec![0; 33], KeyAlgorithm::Secp256k1).err(),
            Some(SignerError::InvalidKey)
        );
    }

    #[test]
    fn rejects_invalid_secp256k1_scalars() {
        let curve_order =
            hex::decode("fffffffffffffffffffffffffffffffebaaedce6af48a03bbfd25e8cd0364141")
                .expect("curve order vector is valid hex");
        for key in [vec![0; 32], curve_order] {
            assert_eq!(
                DerivedKey::from_response(key, vec![0; 33], KeyAlgorithm::Secp256k1).err(),
                Some(SignerError::InvalidKey)
            );
        }
    }

    #[test]
    fn rejects_public_key_mismatch() {
        assert_eq!(
            DerivedKey::from_response(vec![1; 32], vec![0; 33], KeyAlgorithm::Secp256k1).err(),
            Some(SignerError::InvalidKey)
        );
        assert_eq!(
            DerivedKey::from_response(vec![1; 32], vec![0; 32], KeyAlgorithm::Ed25519).err(),
            Some(SignerError::InvalidKey)
        );
    }
}
