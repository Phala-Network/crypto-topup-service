//! Attestation boundary between the API and the dstack attestor.

use std::future::Future;
use std::pin::Pin;

use topup_adapters::attestation::{AttestedWebhookKey, DstackAttestor};

/// Boxed attestation operation suitable for an application-state trait object.
pub type AttestationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AttestationEvidence, AttestationError>> + Send + 'a>>;

/// Failure to collect current attestation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationError {
    /// The configured attestation provider is temporarily unavailable.
    Unavailable,
}

/// What an attestation binds: a nonce, and an account's webhook keys in one mode.
#[derive(Clone, Copy, Debug)]
pub struct AttestationRequest<'a> {
    /// Decoded caller nonce, at most 32 bytes.
    pub nonce: &'a [u8],
    /// The caller's account, `acct_…`.
    pub account: &'a str,
    /// The caller's mode.
    pub livemode: bool,
    /// Key versions that sign the account's deliveries, current first.
    pub versions: &'a [u32],
}

/// Evidence binding a request (`topup_adapters::attestation::report_data`).
#[derive(Clone, Debug)]
pub struct AttestationEvidence {
    /// The requested keys, in the requested order.
    pub webhook_keys: Vec<AttestedWebhookKey>,
    /// The report data the quote carries.
    pub report_data: [u8; 32],
    /// Versioned dstack attestation bytes.
    pub quote: Vec<u8>,
}

/// Runtime source of nonce-bound webhook-key evidence.
pub trait Attestor: Send + Sync {
    /// Derives the requested keys and collects evidence binding them, the account, the mode, and
    /// the nonce.
    fn attest<'a>(&'a self, request: AttestationRequest<'a>) -> AttestationFuture<'a>;
}

impl Attestor for DstackAttestor {
    fn attest<'a>(&'a self, request: AttestationRequest<'a>) -> AttestationFuture<'a> {
        Box::pin(async move {
            let evidence = DstackAttestor::attest(
                self,
                request.nonce,
                request.account,
                request.livemode,
                request.versions,
            )
            .await
            .map_err(|_| AttestationError::Unavailable)?;
            Ok(AttestationEvidence {
                webhook_keys: evidence.webhook_keys,
                report_data: evidence.report_data,
                quote: evidence.quote,
            })
        })
    }
}
