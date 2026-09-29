//! Signing boundary types shared by service adapters.

use secrecy::zeroize::Zeroize;
use secrecy::{ExposeSecret, ExposeSecretMut, SecretBox};

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
/// Domain used to derive the key that tags quotes' and deposit addresses' `client_secret`s.
///
/// Changing the version invalidates every issued secret.
pub const CLIENT_SECRET_KEY_DOMAIN: &str = "client-secret/v1";

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

/// One version of an account's webhook signing key in one mode (design D11).
///
/// Each account has one ed25519 Standard Webhooks `v1a` key per mode, derived from dstack KMS at
/// `settlement/{account}/{live|test}/v{version}`; the service stores no secret. Rotation bumps the
/// version, and the key is a function of the application key and this path alone, so it is stable
/// across releases.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct WebhookKeyId {
    account: String,
    livemode: bool,
    version: u32,
}

impl WebhookKeyId {
    /// The key of `account` (its `acct_` id) in the given mode at `version`.
    ///
    /// Returns `None` unless `account` is an `acct_` id of ASCII letters, digits, and `_` and
    /// `version` is at least 1, so a derivation path never carries a separator or other input.
    #[must_use]
    pub fn new(account: &str, livemode: bool, version: u32) -> Option<Self> {
        let valid = account.len() <= 64
            && account
                .strip_prefix("acct_")
                .is_some_and(|rest| !rest.is_empty())
            && account
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
        (valid && version >= 1).then(|| Self {
            account: account.to_owned(),
            livemode,
            version,
        })
    }

    /// The account's `acct_` id.
    #[must_use]
    pub fn account(&self) -> &str {
        &self.account
    }

    /// Whether this is the account's live-mode key.
    #[must_use]
    pub const fn livemode(&self) -> bool {
        self.livemode
    }

    /// The key version, from 1.
    #[must_use]
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// The dstack KMS derivation path, `settlement/{account}/{live|test}/v{version}`.
    #[must_use]
    pub fn domain(&self) -> String {
        let mode = if self.livemode { "live" } else { "test" };
        format!("settlement/{}/{mode}/v{}", self.account, self.version)
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

/// Signs webhook payloads with per-account, per-mode keys.
///
/// Implementors are safe to move and share across service tasks. The returned futures are not
/// required to be `Send` because the pinned dstack Unix transport does not provide `Send` futures.
#[allow(async_fn_in_trait)]
pub trait Signer: Send + Sync {
    /// Signs payload bytes with ed25519 under the webhook key `key`.
    async fn sign_webhook(
        &self,
        key: &WebhookKeyId,
        payload: &[u8],
    ) -> Result<Ed25519Signature, SignerError>;

    /// Returns the public key of the webhook key `key`.
    async fn webhook_public_key(&self, key: &WebhookKeyId)
    -> Result<Ed25519PublicKey, SignerError>;
}

#[cfg(test)]
mod tests {
    use std::fmt::{Debug, Display};

    use static_assertions::assert_not_impl_any;

    use super::{SecretKey32, WebhookKeyId};

    assert_not_impl_any!(SecretKey32: Debug, Display, serde::Serialize);

    #[test]
    fn secret_key_can_only_be_read_explicitly() {
        let key = SecretKey32::new([7; 32]);
        assert_eq!(key.expose_secret(), &[7; 32]);
    }

    #[test]
    fn webhook_key_domain_is_per_account_mode_and_version() {
        let key = |account, livemode, version| {
            WebhookKeyId::new(account, livemode, version).map(|key| key.domain())
        };
        assert_eq!(
            key("acct_1Ab", true, 1).as_deref(),
            Some("settlement/acct_1Ab/live/v1")
        );
        assert_eq!(
            key("acct_1Ab", false, 2).as_deref(),
            Some("settlement/acct_1Ab/test/v2")
        );
        for (account, version) in [
            ("acct_1Ab", 0),
            ("acct_", 1),
            ("cus_1Ab", 1),
            ("acct_1/../x", 1),
            ("acct_1 b", 1),
        ] {
            assert_eq!(key(account, true, version), None, "{account} v{version}");
        }
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
