//! Attestation boundary used until C11 is available on `main`.

use std::future::Future;
use std::pin::Pin;

use super::models::AttestationResponse;

/// Boxed attestation operation suitable for an application-state trait object.
pub type AttestationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AttestationResponse, AttestationError>> + Send + 'a>>;

/// Failure to collect current attestation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationError {
    /// C11 has not yet supplied a runtime attestor.
    NotConfigured,
    /// The configured attestation provider is temporarily unavailable.
    Unavailable,
}

/// Runtime source of nonce-bound settlement-key evidence.
pub trait Attestor: Send + Sync {
    /// Collects attestation evidence for the decoded nonce bytes.
    fn attest<'a>(&'a self, nonce: &'a [u8]) -> AttestationFuture<'a>;
}

/// Placeholder used until the C11 adapter is merged and wired into `topup run`.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableAttestor;

impl Attestor for UnavailableAttestor {
    fn attest<'a>(&'a self, _nonce: &'a [u8]) -> AttestationFuture<'a> {
        Box::pin(async { Err(AttestationError::NotConfigured) })
    }
}
