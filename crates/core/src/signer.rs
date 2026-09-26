//! Signing boundary types shared by service adapters.

use std::num::NonZeroU32;

use alloy_primitives::{Address, Bytes, U256};
use secrecy::zeroize::Zeroize;
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox};

/// Returns the domain used to derive the transaction-signing key at `version`.
///
/// The version comes from the attested chain configuration, so rotating the operator key is a new
/// configuration version rather than a runtime change.
#[must_use]
pub fn operator_key_domain(version: NonZeroU32) -> String {
    format!("operator/v{version}")
}

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

/// Minimal EIP-1559 transaction request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TxRequest {
    /// EIP-155 chain identifier.
    pub chain_id: u64,
    /// Sender account nonce.
    pub nonce: u64,
    /// Recipient address.
    pub to: Address,
    /// Value in wei.
    pub value: U256,
    /// Contract call data.
    pub data: Bytes,
    /// Maximum gas units.
    pub gas_limit: u64,
    /// Maximum total fee per gas unit.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas unit.
    pub max_priority_fee_per_gas: u128,
}

/// An EIP-2718 encoded signed transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedTx {
    /// Raw signed transaction bytes.
    pub raw_signed_bytes: Bytes,
}

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

/// Signs operator transactions and product settlement payloads.
///
/// Implementors are safe to move and share across service tasks. The returned futures are not
/// required to be `Send` because the pinned dstack Unix transport does not provide `Send` futures.
#[allow(async_fn_in_trait)]
pub trait Signer: Send + Sync {
    /// Signs an EIP-1559 operator transaction.
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError>;

    /// Signs settlement payload bytes with ed25519.
    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError>;

    /// Returns the current operator address.
    async fn operator_address(&self) -> Result<Address, SignerError>;

    /// Returns the current settlement public key.
    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError>;
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};

    use static_assertions::assert_not_impl_any;

    use std::num::NonZeroU32;

    use super::{SecretKey32, operator_key_domain};

    assert_not_impl_any!(SecretKey32: Debug, Display, serde::Serialize);

    #[test]
    fn operator_key_domain_carries_the_configured_version() {
        assert_eq!(operator_key_domain(NonZeroU32::MIN), "operator/v1");
        assert_eq!(
            operator_key_domain(NonZeroU32::new(2).expect("two is non-zero")),
            "operator/v2"
        );
    }

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
