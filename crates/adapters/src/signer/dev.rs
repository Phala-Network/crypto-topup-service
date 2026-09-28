//! In-memory signer for local development and tests.

use sha2::{Digest as _, Sha256};
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SETTLEMENT_KEY_DOMAIN, SecretKey32, Signer, SignerError,
};

use super::{settlement_public_key, sign_settlement};

/// A signer backed by a process-memory settlement key.
pub struct DevSigner {
    settlement_key: SecretKey32,
}

impl DevSigner {
    /// Creates a development signer from an explicit settlement key.
    #[must_use]
    pub const fn new(settlement_key: SecretKey32) -> Self {
        Self { settlement_key }
    }

    /// Derives the development settlement key from `seed` with the dstack signer's domain.
    ///
    /// The key is `SHA-256(seed || "settlement/v1")`. This mirrors the domain separation only; it
    /// is not dstack's key derivation.
    #[must_use]
    pub fn derive(seed: &SecretKey32) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(seed.expose_secret());
        hasher.update(SETTLEMENT_KEY_DOMAIN.as_bytes());
        Self::new(SecretKey32::new(hasher.finalize().into()))
    }
}

impl Signer for DevSigner {
    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(sign_settlement(&self.settlement_key, payload))
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(settlement_public_key(&self.settlement_key))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use topup_core::{SecretKey32, Signer as _};

    use super::DevSigner;

    #[tokio::test]
    async fn signs_and_verifies_settlement_payloads() {
        let signer = DevSigner::new(SecretKey32::new([2; 32]));
        let payload = b"settlement payload";
        let public_key = signer
            .settlement_public_key()
            .await
            .expect("development key should be valid");
        let signature = signer
            .sign_settlement(payload)
            .await
            .expect("development signing should succeed");
        let verifying_key = VerifyingKey::from_bytes(&public_key.0)
            .expect("development public key should be valid");

        assert!(
            verifying_key
                .verify(payload, &Signature::from_bytes(&signature.0))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn derived_key_is_deterministic_per_seed() {
        let key = |seed| async move {
            DevSigner::derive(&SecretKey32::new([seed; 32]))
                .settlement_public_key()
                .await
                .expect("derived key is valid")
        };
        assert_eq!(key(5).await, key(5).await);
        assert_ne!(key(5).await, key(6).await);
    }
}
