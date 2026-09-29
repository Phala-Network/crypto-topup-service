//! Self-authenticating `client_secret`s of quotes and deposit addresses (architecture §12).
//!
//! A secret is `{id}_secret_{nonce}{tag}`: `id` is the object's public id (`qt_…` or `da_…`),
//! `nonce` 16 random bytes, and `tag` the first 16 bytes of HMAC-SHA256, under the key derived
//! from dstack at [`topup_core::CLIENT_SECRET_KEY_DOMAIN`] like the service's other keys, of
//! everything before it; both are lowercase hex.
//! The tag is checked in memory in constant time, so a forged or malformed secret is refused
//! without touching the database. The database still stores each issued secret's SHA-256: a
//! secret is valid only while its row is, which lets a deposit address retire its old secrets.

use hmac::{Hmac, KeyInit, Mac as _};
use rand::TryRng as _;
use rand::rngs::SysRng;
use sha2::Sha256;
use topup_core::SecretKey32;

const SEPARATOR: &str = "_secret_";
const NONCE_BYTES: usize = 16;
const TAG_BYTES: usize = 16;

/// The key that tags and checks client secrets.
pub struct ClientSecretKey(SecretKey32);

impl std::fmt::Debug for ClientSecretKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ClientSecretKey(..)")
    }
}

/// The OS RNG failed, so no secret was issued.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("OS RNG failed; no client secret issued")]
pub struct EntropyUnavailable;

impl ClientSecretKey {
    /// A key from its 32 bytes, as derived at [`topup_core::CLIENT_SECRET_KEY_DOMAIN`].
    #[must_use]
    pub const fn new(key: SecretKey32) -> Self {
        Self(key)
    }

    /// A random key of this process alone, for tests: secrets it issues die with the process.
    ///
    /// # Panics
    ///
    /// When the OS RNG fails, as a UUID v4 would.
    #[must_use]
    pub fn ephemeral() -> Self {
        let mut key = [0_u8; 32];
        SysRng
            .try_fill_bytes(&mut key)
            .expect("the OS RNG provides a client-secret key");
        Self(SecretKey32::new(key))
    }

    /// Issues a new secret of the object whose public id is `id`.
    pub fn issue(&self, id: &str) -> Result<String, EntropyUnavailable> {
        let mut nonce = [0_u8; NONCE_BYTES];
        SysRng
            .try_fill_bytes(&mut nonce)
            .map_err(|_| EntropyUnavailable)?;
        let signed = format!("{id}{SEPARATOR}{}", hex::encode(nonce));
        let tag = self.mac(&signed).finalize().into_bytes();
        Ok(format!("{signed}{}", hex::encode(&tag[..TAG_BYTES])))
    }

    /// Whether `secret` is well formed for the object whose public id is `id` and carries this
    /// key's tag. It does not say whether the secret is still valid: the database does.
    #[must_use]
    pub fn verify(&self, id: &str, secret: &str) -> bool {
        let Some(rest) = secret
            .strip_prefix(id)
            .and_then(|rest| rest.strip_prefix(SEPARATOR))
        else {
            return false;
        };
        let lowercase_hex = rest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        if rest.len() != 2 * (NONCE_BYTES + TAG_BYTES) || !lowercase_hex {
            return false;
        }
        let (signed, tag) = secret.split_at(secret.len() - 2 * TAG_BYTES);
        let Ok(tag) = hex::decode(tag) else {
            return false;
        };
        self.mac(signed).verify_truncated_left(&tag).is_ok()
    }

    fn mac(&self, message: &str) -> Hmac<Sha256> {
        let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(self.0.expose_secret())
            .expect("HMAC takes a key of any length");
        mac.update(message.as_bytes());
        mac
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "qt_0123456789abcdef0123456789abcdef";

    fn key(byte: u8) -> ClientSecretKey {
        ClientSecretKey::new(SecretKey32::new([byte; 32]))
    }

    #[test]
    fn an_issued_secret_verifies_for_its_object_and_key_only() {
        let secret = key(1).issue(ID).expect("secret");
        assert_eq!(secret.len(), ID.len() + SEPARATOR.len() + 64);
        assert!(key(1).verify(ID, &secret));
        assert!(!key(2).verify(ID, &secret));
        let other = "qt_fedcba9876543210fedcba9876543210";
        assert!(!key(1).verify(other, &secret.replacen(ID, other, 1)));
        assert_ne!(key(1).issue(ID).expect("secret"), secret, "nonces differ");
    }

    #[test]
    fn a_forged_or_malformed_secret_is_refused() {
        let secret = key(1).issue(ID).expect("secret");
        let mut flipped = secret.clone().into_bytes();
        let last = flipped.len() - 1;
        flipped[last] = if flipped[last] == b'0' { b'1' } else { b'0' };
        let flipped = String::from_utf8(flipped).expect("ascii");
        for forged in [
            flipped,
            secret.to_uppercase(),
            secret[..secret.len() - 2].to_owned(),
            format!("{secret}00"),
            format!("{ID}{SEPARATOR}{}", "0".repeat(48)),
            String::new(),
        ] {
            assert!(!key(1).verify(ID, &forged), "{forged}");
        }
    }
}
