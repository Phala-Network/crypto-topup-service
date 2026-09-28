//! Attestation boundary between the API and the dstack attestor.

use std::future::Future;
use std::pin::Pin;

use topup_adapters::attestation::DstackAttestor;
use topup_core::SETTLEMENT_KEY_DOMAIN;

use super::models::AttestationResponse;

/// Boxed attestation operation suitable for an application-state trait object.
pub type AttestationFuture<'a> =
    Pin<Box<dyn Future<Output = Result<AttestationResponse, AttestationError>> + Send + 'a>>;

/// Failure to collect current attestation evidence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttestationError {
    /// The configured attestation provider is temporarily unavailable.
    Unavailable,
}

/// Runtime source of nonce-bound settlement-key evidence.
pub trait Attestor: Send + Sync {
    /// Collects attestation evidence binding the decoded nonce bytes and the settlement key.
    fn attest<'a>(&'a self, nonce: &'a [u8]) -> AttestationFuture<'a>;
}

impl Attestor for DstackAttestor {
    fn attest<'a>(&'a self, nonce: &'a [u8]) -> AttestationFuture<'a> {
        Box::pin(async move {
            let evidence = DstackAttestor::attest(self, nonce)
                .await
                .map_err(|_| AttestationError::Unavailable)?;
            Ok(AttestationResponse {
                keyid: SETTLEMENT_KEY_DOMAIN.to_owned(),
                settlement_pubkey: hex::encode(evidence.settlement_public_key.0),
                report_data: hex::encode(evidence.report_data),
                quote: hex::encode(evidence.quote),
            })
        })
    }
}
