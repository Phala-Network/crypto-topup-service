use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use topup_core::{Signer, SignerError, WebhookKeyId};

/// Standard Webhooks metadata and asymmetric `v1a` signature.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SignedWebhook {
    /// Stable event identifier: the `evt_` id, or the bare UUID of a format-1 event.
    pub id: String,
    /// Attempt timestamp as integer Unix seconds.
    pub timestamp: String,
    /// One `v1a,<base64>` entry per key, space-delimited: during a key rotation the receiver
    /// accepts the delivery if any entry verifies with a key it pinned.
    pub signature: String,
}

impl SignedWebhook {
    /// Signs `{id}.{timestamp}.{body}` exactly as sent on the wire with each of `keys`, which
    /// must not be empty.
    pub async fn new(
        signer: &impl Signer,
        keys: &[WebhookKeyId],
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
        if keys.is_empty() {
            return Err(SignerError::KeyUnavailable);
        }
        let mut entries = Vec::with_capacity(keys.len());
        for key in keys {
            let signature = signer.sign_webhook(key, &content).await?;
            entries.push(format!("v1a,{}", STANDARD.encode(signature.0)));
        }

        Ok(Self {
            id,
            timestamp,
            signature: entries.join(" "),
        })
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer as _, SigningKey};
    use topup_core::{Ed25519PublicKey, Ed25519Signature, Signer, SignerError, WebhookKeyId};

    use uuid::Uuid;

    use super::*;

    /// Signs with `[7; 32]` for version 1 and `[8; 32]` for any other version.
    struct FixedSigner;

    impl FixedSigner {
        fn key(key: &WebhookKeyId) -> SigningKey {
            SigningKey::from_bytes(&[if key.version() == 1 { 7 } else { 8 }; 32])
        }
    }

    impl Signer for FixedSigner {
        async fn sign_webhook(
            &self,
            key: &WebhookKeyId,
            content: &[u8],
        ) -> Result<Ed25519Signature, SignerError> {
            Ok(Ed25519Signature(Self::key(key).sign(content).to_bytes()))
        }

        async fn webhook_public_key(
            &self,
            key: &WebhookKeyId,
        ) -> Result<Ed25519PublicKey, SignerError> {
            Ok(Ed25519PublicKey(Self::key(key).verifying_key().to_bytes()))
        }
    }

    fn key(version: u32) -> WebhookKeyId {
        WebhookKeyId::new("acct_a", false, version).expect("valid key id")
    }

    #[tokio::test]
    async fn asymmetric_header_matches_fixed_standard_webhooks_vector() {
        let signer = FixedSigner;
        let event_id = Uuid::parse_str("018d5f8e-8a7b-7d65-bc44-2c4f5f0a6d31")
            .expect("fixed UUID should parse");
        let body = br#"{"type":"deposit.confirmed","data":{"deposit_id":"dep_123"}}"#;
        let signed = SignedWebhook::new(
            &signer,
            &[key(1)],
            &event_id.to_string(),
            1_674_087_231,
            body,
        )
        .await
        .expect("fixed signer should sign");

        assert_eq!(signed.id, event_id.to_string());
        assert_eq!(signed.timestamp, "1674087231");
        assert_eq!(
            signed.signature,
            "v1a,0thypM6abf9ly803QGttAKGQfPFKHiwgpxF+b4zWDUCycKswAoJ848WmI7VKQBw8NIWO74zYeRvd7vw/cGOZBw=="
        );
    }

    #[tokio::test]
    async fn every_key_signs_during_a_rotation_and_none_is_refused() {
        let signer = FixedSigner;
        let rotating = SignedWebhook::new(&signer, &[key(2), key(1)], "evt_1", 1, b"{}")
            .await
            .expect("fixed signer should sign");
        let mut single = Vec::new();
        for version in [2, 1] {
            single.push(
                SignedWebhook::new(&signer, &[key(version)], "evt_1", 1, b"{}")
                    .await
                    .expect("fixed signer should sign")
                    .signature,
            );
        }
        assert_eq!(rotating.signature, single.join(" "));
        assert_eq!(
            SignedWebhook::new(&signer, &[], "evt_1", 1, b"{}").await,
            Err(SignerError::KeyUnavailable)
        );
    }
}
