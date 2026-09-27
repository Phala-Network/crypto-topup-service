use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use topup_core::{Signer, SignerError};

/// Standard Webhooks metadata and asymmetric `v1a` signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedWebhook {
    /// Stable event identifier: the `evt_` id, or the bare UUID of a format-1 event.
    pub id: String,
    /// Attempt timestamp as integer Unix seconds.
    pub timestamp: String,
    /// Space-compatible signature entry in `v1a,<base64>` form.
    pub signature: String,
}

impl SignedWebhook {
    /// Signs `{id}.{timestamp}.{body}` exactly as sent on the wire.
    pub async fn new(
        signer: &impl Signer,
        event_id: &str,
        timestamp: i64,
        body: &[u8],
    ) -> Result<Self, SignerError> {
        let id = event_id.to_owned();
        let timestamp = timestamp.to_string();
        let mut content = Vec::with_capacity(id.len() + timestamp.len() + body.len() + 2);
        content.extend_from_slice(id.as_bytes());
        content.push(b'.');
        content.extend_from_slice(timestamp.as_bytes());
        content.push(b'.');
        content.extend_from_slice(body);
        let signature = signer.sign_settlement(&content).await?;

        Ok(Self {
            id,
            timestamp,
            signature: format!("v1a,{}", STANDARD.encode(signature.0)),
        })
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use ed25519_dalek::{Signer as _, SigningKey};
    use topup_core::{
        Ed25519PublicKey, Ed25519Signature, SignedTx, Signer, SignerError, TxRequest,
    };

    use uuid::Uuid;

    use super::*;

    struct FixedSigner(SigningKey);

    impl Signer for FixedSigner {
        async fn sign_operator_tx(&self, _tx: TxRequest) -> Result<SignedTx, SignerError> {
            Err(SignerError::SigningFailed)
        }

        async fn sign_settlement(&self, content: &[u8]) -> Result<Ed25519Signature, SignerError> {
            Ok(Ed25519Signature(self.0.sign(content).to_bytes()))
        }

        async fn operator_address(&self) -> Result<Address, SignerError> {
            Err(SignerError::KeyUnavailable)
        }

        async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
            Ok(Ed25519PublicKey(self.0.verifying_key().to_bytes()))
        }
    }

    #[tokio::test]
    async fn asymmetric_header_matches_fixed_standard_webhooks_vector() {
        let signer = FixedSigner(SigningKey::from_bytes(&[7_u8; 32]));
        let event_id = Uuid::parse_str("018d5f8e-8a7b-7d65-bc44-2c4f5f0a6d31")
            .expect("fixed UUID should parse");
        let body = br#"{"type":"deposit.confirmed","data":{"deposit_id":"dep_123"}}"#;
        let signed = SignedWebhook::new(&signer, &event_id.to_string(), 1_674_087_231, body)
            .await
            .expect("fixed signer should sign");

        assert_eq!(signed.id, event_id.to_string());
        assert_eq!(signed.timestamp, "1674087231");
        assert_eq!(
            signed.signature,
            "v1a,0thypM6abf9ly803QGttAKGQfPFKHiwgpxF+b4zWDUCycKswAoJ848WmI7VKQBw8NIWO74zYeRvd7vw/cGOZBw=="
        );
    }
}
