//! In-memory signer for local development and tests.

use std::num::NonZeroU32;

use sha2::{Digest as _, Sha256};
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SETTLEMENT_KEY_DOMAIN, SecretKey32, SignedTx, Signer,
    SignerError, TxRequest, operator_key_domain,
};

use super::{operator_address, settlement_public_key, sign_operator_tx, sign_settlement};

/// A signer backed by process-memory keys.
pub struct DevSigner {
    operator_key: SecretKey32,
    settlement_key: SecretKey32,
}

impl DevSigner {
    /// Creates a development signer from explicit keys.
    #[must_use]
    pub fn new(operator_key: SecretKey32, settlement_key: SecretKey32) -> Self {
        Self {
            operator_key,
            settlement_key,
        }
    }

    /// Derives development keys from `seed` with the same domains as the dstack signer.
    ///
    /// Each key is `SHA-256(seed || domain)`: the operator key uses
    /// `operator/v{operator_key_version}` and the settlement key `settlement/v1`. This mirrors the
    /// domain separation only; it is not dstack's key derivation.
    #[must_use]
    pub fn derive(seed: &SecretKey32, operator_key_version: NonZeroU32) -> Self {
        Self::new(
            derive_key(seed, &operator_key_domain(operator_key_version)),
            derive_key(seed, SETTLEMENT_KEY_DOMAIN),
        )
    }
}

fn derive_key(seed: &SecretKey32, domain: &str) -> SecretKey32 {
    let mut hasher = Sha256::new();
    hasher.update(seed.expose_secret());
    hasher.update(domain.as_bytes());
    SecretKey32::new(hasher.finalize().into())
}

impl Signer for DevSigner {
    async fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError> {
        sign_operator_tx(&self.operator_key, tx)
    }

    async fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(sign_settlement(&self.settlement_key, payload))
    }

    async fn operator_address(&self) -> Result<alloy_primitives::Address, SignerError> {
        operator_address(&self.operator_key)
    }

    async fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(settlement_public_key(&self.settlement_key))
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{TxEnvelope, TxType, transaction::SignerRecoverable as _};
    use alloy_eips::eip2718::{Decodable2718, Typed2718 as _};
    use alloy_primitives::{Address, Bytes, TxKind, U256};
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use std::num::NonZeroU32;

    use sha2::{Digest as _, Sha256};
    use topup_core::{SecretKey32, Signer as _, TxRequest};

    use super::DevSigner;

    fn signer() -> DevSigner {
        DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([2; 32]))
    }

    fn transaction() -> TxRequest {
        TxRequest {
            chain_id: 1,
            nonce: 7,
            to: Address::from([3; 20]),
            value: U256::from(42_u8),
            data: Bytes::from_static(&[0xde, 0xad, 0xbe, 0xef]),
            gas_limit: 75_000,
            max_fee_per_gas: 30_000_000_000,
            max_priority_fee_per_gas: 2_000_000_000,
        }
    }

    #[tokio::test]
    async fn derived_operator_key_follows_the_version_domain() {
        let seed = SecretKey32::new([5; 32]);
        let v1 = DevSigner::derive(&seed, NonZeroU32::MIN);
        let v2 = DevSigner::derive(&seed, NonZeroU32::new(2).expect("two is non-zero"));
        let expected = |domain: &str| {
            let mut hasher = Sha256::new();
            hasher.update([5; 32]);
            hasher.update(domain.as_bytes());
            DevSigner::new(
                SecretKey32::new(hasher.finalize().into()),
                SecretKey32::new([0; 32]),
            )
        };

        let v1_operator = v1.operator_address().await.expect("v1 key is valid");
        let v2_operator = v2.operator_address().await.expect("v2 key is valid");
        assert_ne!(v1_operator, v2_operator);
        assert_eq!(
            Some(v1_operator),
            expected("operator/v1").operator_address().await.ok()
        );
        assert_eq!(
            Some(v2_operator),
            expected("operator/v2").operator_address().await.ok()
        );
        assert_eq!(
            v1.settlement_public_key().await,
            v2.settlement_public_key().await
        );
    }

    #[tokio::test]
    async fn signs_and_verifies_settlement_payloads() {
        let signer = signer();
        let payload = b"settlement payload";
        let public_key = signer
            .settlement_public_key()
            .await
            .expect("development key should be valid");
        let signature = signer
            .sign_settlement(payload)
            .await
            .expect("development signing should succeed");
        let verifying_key = VerifyingKey::from_bytes(&public_key.0)
            .expect("development public key should be valid");

        assert!(
            verifying_key
                .verify(payload, &Signature::from_bytes(&signature.0))
                .is_ok()
        );
    }

    #[tokio::test]
    async fn signed_transaction_preserves_fields_and_recovers_the_operator() {
        let signer = signer();
        let request = transaction();
        let expected = signer
            .operator_address()
            .await
            .expect("development key should be valid");
        let signed = signer
            .sign_operator_tx(request.clone())
            .await
            .expect("development signing should succeed");
        let envelope = TxEnvelope::decode_2718_exact(&signed.raw_signed_bytes)
            .expect("signed transaction should decode");
        let recovered = envelope
            .recover_signer()
            .expect("signed transaction should recover");

        assert_eq!(envelope.ty(), TxType::Eip1559 as u8);
        assert!(envelope.is_eip1559());
        let transaction = envelope
            .as_eip1559()
            .expect("type-2 envelope must contain an EIP-1559 transaction")
            .tx();
        assert_eq!(transaction.chain_id, request.chain_id);
        assert_eq!(transaction.nonce, request.nonce);
        assert_eq!(transaction.to, TxKind::Call(request.to));
        assert_eq!(transaction.value, request.value);
        assert!(!transaction.value.is_zero());
        assert_eq!(transaction.input, request.data);
        assert_eq!(transaction.gas_limit, request.gas_limit);
        assert_eq!(transaction.max_fee_per_gas, request.max_fee_per_gas);
        assert_eq!(
            transaction.max_priority_fee_per_gas,
            request.max_priority_fee_per_gas
        );
        assert_eq!(recovered, expected);
    }
}
