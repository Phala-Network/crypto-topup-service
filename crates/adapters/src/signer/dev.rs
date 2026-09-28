//! In-memory signer for local development and tests.

use sha2::{Digest as _, Sha256};
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SecretKey32, Signer, SignerError, WebhookKeyId,
};

use super::{ed25519_public_key, sign_ed25519};

/// A signer deriving every webhook key from a process-memory seed.
pub struct DevSigner {
    seed: SecretKey32,
}

impl DevSigner {
    /// Creates a development signer whose keys derive from `seed` with the dstack signer's
    /// domains.
    ///
    /// A key is `SHA-256(seed || domain)`, for example `settlement/acct_…/test/v1`. This mirrors
    /// the domain separation only; it is not dstack's key derivation.
    #[must_use]
    pub fn derive(seed: &SecretKey32) -> Self {
        Self {
            seed: SecretKey32::new(*seed.expose_secret()),
        }
    }

    fn key(&self, key: &WebhookKeyId) -> SecretKey32 {
        let mut hasher = Sha256::new();
        hasher.update(self.seed.expose_secret());
        hasher.update(key.domain().as_bytes());
        SecretKey32::new(hasher.finalize().into())
    }
}

impl Signer for DevSigner {
    async fn sign_webhook(
        &self,
        key: &WebhookKeyId,
        payload: &[u8],
    ) -> Result<Ed25519Signature, SignerError> {
        Ok(sign_ed25519(&self.key(key), payload))
    }

    async fn webhook_public_key(
        &self,
        key: &WebhookKeyId,
    ) -> Result<Ed25519PublicKey, SignerError> {
        Ok(ed25519_public_key(&self.key(key)))
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use topup_core::{SecretKey32, Signer as _, WebhookKeyId};

    use super::DevSigner;

    fn key(account: &str, livemode: bool, version: u32) -> WebhookKeyId {
        WebhookKeyId::new(account, livemode, version).expect("valid key id")
    }

    #[tokio::test]
    async fn signs_and_verifies_webhook_payloads() {
        let signer = DevSigner::derive(&SecretKey32::new([2; 32]));
        let id = key("acct_a", false, 1);
        let payload = b"webhook payload";
        let public_key = signer
            .webhook_public_key(&id)
            .await
            .expect("development key should be valid");
        let signature = signer
            .sign_webhook(&id, payload)
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
    async fn keys_are_deterministic_and_distinct_per_account_mode_and_version() {
        let public = |seed, id: WebhookKeyId| async move {
            DevSigner::derive(&SecretKey32::new([seed; 32]))
                .webhook_public_key(&id)
                .await
                .expect("derived key is valid")
        };
        let base = public(5, key("acct_a", true, 1)).await;
        assert_eq!(base, public(5, key("acct_a", true, 1)).await);
        for other in [
            public(6, key("acct_a", true, 1)).await,
            public(5, key("acct_b", true, 1)).await,
            public(5, key("acct_a", false, 1)).await,
            public(5, key("acct_a", true, 2)).await,
        ] {
            assert_ne!(base, other);
        }
    }
}
