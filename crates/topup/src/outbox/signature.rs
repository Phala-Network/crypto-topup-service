use std::error::Error;
use std::fmt::{Display, Formatter};

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use uuid::Uuid;

/// Error returned when the settlement key cannot sign a webhook.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventSignError {
    message: &'static str,
}

impl EventSignError {
    /// Creates a non-sensitive signing error suitable for logs and retry records.
    #[must_use]
    pub const fn new(message: &'static str) -> Self {
        Self { message }
    }
}

impl Display for EventSignError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for EventSignError {}

/// Minimal settlement-key boundary used until C11's signer lands on `main`.
pub trait EventSigner: Send + Sync {
    /// Signs the exact Standard Webhooks content bytes with ed25519.
    fn sign_event(&self, content: &[u8]) -> Result<[u8; 64], EventSignError>;
}

/// Standard Webhooks metadata and asymmetric `v1a` signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedWebhook {
    /// Stable event identifier.
    pub id: String,
    /// Attempt timestamp as integer Unix seconds.
    pub timestamp: String,
    /// Space-compatible signature entry in `v1a,<base64>` form.
    pub signature: String,
}

impl SignedWebhook {
    /// Signs `{id}.{timestamp}.{body}` exactly as sent on the wire.
    pub fn new(
        signer: &impl EventSigner,
        event_id: Uuid,
        timestamp: i64,
        body: &[u8],
    ) -> Result<Self, EventSignError> {
        let id = event_id.to_string();
        let timestamp = timestamp.to_string();
        let mut content = Vec::with_capacity(id.len() + timestamp.len() + body.len() + 2);
        content.extend_from_slice(id.as_bytes());
        content.push(b'.');
        content.extend_from_slice(timestamp.as_bytes());
        content.push(b'.');
        content.extend_from_slice(body);
        let signature = signer.sign_event(&content)?;

        Ok(Self {
            id,
            timestamp,
            signature: format!("v1a,{}", STANDARD.encode(signature)),
        })
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};

    use super::*;

    struct FixedSigner(SigningKey);

    impl EventSigner for FixedSigner {
        fn sign_event(&self, content: &[u8]) -> Result<[u8; 64], EventSignError> {
            Ok(self.0.sign(content).to_bytes())
        }
    }

    #[test]
    fn asymmetric_header_matches_fixed_standard_webhooks_vector() {
        let signer = FixedSigner(SigningKey::from_bytes(&[7_u8; 32]));
        let event_id = Uuid::parse_str("018d5f8e-8a7b-7d65-bc44-2c4f5f0a6d31")
            .expect("fixed UUID should parse");
        let body = br#"{"type":"deposit.confirmed","data":{"deposit_id":"dep_123"}}"#;
        let signed = SignedWebhook::new(&signer, event_id, 1_674_087_231, body)
            .expect("fixed signer should sign");

        assert_eq!(signed.id, event_id.to_string());
        assert_eq!(signed.timestamp, "1674087231");
        assert_eq!(
            signed.signature,
            "v1a,0thypM6abf9ly803QGttAKGQfPFKHiwgpxF+b4zWDUCycKswAoJ848WmI7VKQBw8NIWO74zYeRvd7vw/cGOZBw=="
        );
    }
}
