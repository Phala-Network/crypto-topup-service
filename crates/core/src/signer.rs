//! Signing boundary types shared by service adapters.

use secrecy::zeroize::Zeroize;
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox};

/// Domain used to derive the settlement-signing key.
pub const SETTLEMENT_KEY_DOMAIN: &str = "settlement/v1";
/// Domain used to derive the WAL-G backup-encryption key, the one key of the backup prefix.
///
/// Every existing backup is encrypted under this path, so it never changes; a new key goes with a
/// new `WALG_S3_PREFIX` (deploy/RESTORE.md, "Backup key").
pub const BACKUP_KEY_DOMAIN: &str = "backup/v1";
/// Domain used to derive the database owner (`postgres`) password.
///
/// Every CVM of one dstack application derives the same value, so a replacement CVM logs in to a
/// restored cluster without a supplied secret. Changing the version requires `ALTER ROLE`.
pub const DB_OWNER_KEY_DOMAIN: &str = "db/owner/v1";
/// Domain used to derive the application login (`topup_service`) password.
pub const DB_APP_KEY_DOMAIN: &str = "db/app/v1";

/// A 32-byte secret which is zeroized when dropped.
pub struct SecretKey32(SecretBox<[u8; 32]>);

impl SecretKey32 {
    /// Moves key bytes into protected storage and clears the input buffer.
    #[must_use]
    pub fn new(mut bytes: [u8; 32]) -> Self {
        let mut secret: SecretBox<[u8; 32]> = SecretBox::default();
        secret.expose_secret_mut().copy_from_slice(&bytes);
        bytes.zeroize();
        Self(secret)
    }

    /// Copies exactly 32 bytes directly into protected storage.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 32 {
            return None;
        }
        let mut secret: SecretBox<[u8; 32]> = SecretBox::default();
        secret.expose_secret_mut().copy_from_slice(bytes);
        Some(Self(secret))
    }

    /// Exposes the secret to a cryptographic implementation.
    #[must_use]
    pub fn expose_secret(&self) -> &[u8; 32] {
        self.0.expose_secret()
    }
}

/// A raw ed25519 public key.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ed25519PublicKey(pub [u8; 32]);

/// A raw ed25519 signature.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Ed25519Signature(pub [u8; 64]);

/// A signer boundary failure safe to expose to service callers.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum SignerError {
    /// Key derivation or retrieval failed.
    #[error("signing key is unavailable")]
    KeyUnavailable,
    /// Returned key material had an invalid shape or scalar.
    #[error("signing key is invalid")]
    InvalidKey,
    /// The cryptographic signing operation failed.
    #[error("signing operation failed")]
    SigningFailed,
}

/// Signs settlement payloads.
///
/// Implementors are safe to move and share across service tasks. The returned futures are not
/// required to be `Send` because the pinned dstack Unix transport does not provide `Send` futures.
#[allow(async_fn_in_trait)]
pub trait Signer: Send + Sync {
    /// Signs settlement payload bytes with ed25519.
    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError>;

    /// Returns the current settlement public key.
    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError>;
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};

    use static_assertions::assert_not_impl_any;

    use super::SecretKey32;

    assert_not_impl_any!(SecretKey32: Debug, Display, serde::Serialize);

    #[test]
    fn secret_key_can_only_be_read_explicitly() {
        let key = SecretKey32::new([7; 32]);
        assert_eq!(key.expose_secret(), &[7; 32]);
    }

    #[test]
    fn secret_key_slice_constructor_enforces_length() {
        assert!(SecretKey32::from_slice(&[7; 31]).is_none());
        assert_eq!(
            SecretKey32::from_slice(&[7; 32])
                .expect("32 bytes are valid")
                .expose_secret(),
            &[7; 32]
        );
    }
}
