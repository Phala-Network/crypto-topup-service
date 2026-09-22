//! Standalone product settlement conformance suite and reference endpoint.

pub mod reference;
pub mod report;
pub mod suite;

use std::fs;
use std::num::NonZeroUsize;
use std::path::Path;
use std::time::Duration;

use alloy_primitives::Address;
use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signer as _, SigningKey};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::{Ed25519PublicKey, Ed25519Signature, SignedTx, Signer, SignerError, TxRequest};

/// Seed used by the explicitly requested `dev` signing-key mode.
pub const DEV_SETTLEMENT_SEED: [u8; 32] = [7; 32];

/// Loads a raw or hex-encoded ed25519 seed, or the literal `dev` fixture.
pub fn load_seed(value: &str) -> Result<[u8; 32]> {
    if value == "dev" {
        return Ok(DEV_SETTLEMENT_SEED);
    }
    let bytes = fs::read(Path::new(value)).with_context(|| format!("read signing key {value}"))?;
    if let Ok(seed) = <[u8; 32]>::try_from(bytes.as_slice()) {
        return Ok(seed);
    }
    let text = std::str::from_utf8(&bytes)
        .context("signing key must be 32 raw bytes or 64 hexadecimal characters")?
        .trim();
    let decoded = hex::decode(text).context("decode signing key hex")?;
    <[u8; 32]>::try_from(decoded.as_slice())
        .map_err(|_| anyhow::anyhow!("signing key must contain exactly 32 bytes"))
}

/// Starts the production signer adapter around one settlement seed.
pub fn signer_handle(seed: [u8; 32]) -> Result<SignerHandle> {
    let capacity = NonZeroUsize::new(32).context("signer queue capacity is non-zero")?;
    SignerHandle::spawn(
        SeedSigner(SigningKey::from_bytes(&seed)),
        capacity,
        Duration::from_secs(5),
    )
    .context("start signer actor")
}

struct SeedSigner(SigningKey);

impl Signer for SeedSigner {
    async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
        Err(SignerError::KeyUnavailable)
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(Ed25519Signature(self.0.sign(payload).to_bytes()))
    }

    async fn operator_address(&self) -> Result<Address, SignerError> {
        Err(SignerError::KeyUnavailable)
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
    }
}

/// Rejects an unusable key identifier before it reaches an HTTP header.
pub fn validate_keyid(keyid: &str) -> Result<()> {
    if keyid.is_empty() || keyid.contains(['"', '\\', '\n', '\r']) {
        bail!("keyid must be a non-empty RFC 8941 string without escapes");
    }
    Ok(())
}
