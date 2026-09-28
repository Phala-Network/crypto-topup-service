//! Signing adapters.

use alloy_signer_local::PrivateKeySigner;
use ed25519_dalek::{Signer as _, SigningKey};
use topup_core::{Ed25519PublicKey, Ed25519Signature, SecretKey32, SignerError};
use zeroize::Zeroizing;

pub mod actor;
pub mod dstack;

#[cfg(any(test, feature = "dev-signer"))]
mod dev;
#[cfg(any(test, feature = "dev-signer"))]
pub use dev::DevSigner;

/// Checks that `secret` is a secp256k1 scalar: non-zero and below the curve order.
fn validate_secp256k1(secret: &SecretKey32) -> Result<(), SignerError> {
    PrivateKeySigner::from_slice(secret.expose_secret())
        .map(drop)
        .map_err(|_| SignerError::InvalidKey)
}

fn sign_ed25519(secret: &SecretKey32, payload: &[u8]) -> Ed25519Signature {
    let signing_key = ed25519_signing_key(secret);
    Ed25519Signature(signing_key.sign(payload).to_bytes())
}

pub(crate) fn ed25519_public_key(secret: &SecretKey32) -> Ed25519PublicKey {
    let signing_key = ed25519_signing_key(secret);
    Ed25519PublicKey(signing_key.verifying_key().to_bytes())
}

fn ed25519_signing_key(secret: &SecretKey32) -> SigningKey {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    bytes.copy_from_slice(secret.expose_secret());
    SigningKey::from_bytes(&bytes)
}
