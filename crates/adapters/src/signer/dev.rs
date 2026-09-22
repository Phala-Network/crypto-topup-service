//! In-memory signer for local development and tests.

use topup_core::{
    Ed25519PublicKey, Ed25519Signature, EvmAddress, SecretKey32, SignedTx, Signer, SignerError,
    TxRequest,
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
}

impl Signer for DevSigner {
    fn sign_operator_tx(&self, tx: TxRequest) -> Result<SignedTx, SignerError> {
        sign_operator_tx(&self.operator_key, tx)
    }

    fn sign_settlement(&self, payload: &[u8]) -> Result<Ed25519Signature, SignerError> {
        Ok(sign_settlement(&self.settlement_key, payload))
    }

    fn operator_address(&self) -> Result<EvmAddress, SignerError> {
        operator_address(&self.operator_key)
    }

    fn settlement_public_key(&self) -> Result<Ed25519PublicKey, SignerError> {
        Ok(settlement_public_key(&self.settlement_key))
    }
}

#[cfg(test)]
mod tests {
    use alloy_consensus::{TxEnvelope, transaction::SignerRecoverable as _};
    use alloy_eips::eip2718::Decodable2718;
    use ed25519_dalek::{Signature, Verifier as _, VerifyingKey};
    use topup_core::{EvmAddress, SecretKey32, Signer as _, TxRequest};

    use super::DevSigner;

    fn signer() -> DevSigner {
        DevSigner::new(SecretKey32::new([1; 32]), SecretKey32::new([2; 32]))
    }

    fn transaction() -> TxRequest {
        TxRequest {
            chain_id: 1,
            nonce: 7,
            to: EvmAddress([3; 20]),
            value: [0; 32],
            data: vec![0xde, 0xad, 0xbe, 0xef],
            gas_limit: 75_000,
            max_fee_per_gas: 30_000_000_000,
            max_priority_fee_per_gas: 2_000_000_000,
        }
    }

    #[test]
    fn signs_and_verifies_settlement_payloads() {
        let signer = signer();
        let payload = b"settlement payload";
        let public_key = signer
            .settlement_public_key()
            .expect("development key should be valid");
        let signature = signer
            .sign_settlement(payload)
            .expect("development signing should succeed");
        let verifying_key = VerifyingKey::from_bytes(&public_key.0)
            .expect("development public key should be valid");

        assert!(
            verifying_key
                .verify(payload, &Signature::from_bytes(&signature.0))
                .is_ok()
        );
    }

    #[test]
    fn signed_transaction_recovers_the_operator() {
        let signer = signer();
        let expected = signer
            .operator_address()
            .expect("development key should be valid");
        let signed = signer
            .sign_operator_tx(transaction())
            .expect("development signing should succeed");
        let envelope = TxEnvelope::decode_2718_exact(&signed.raw_signed_bytes)
            .expect("signed transaction should decode");
        let recovered = envelope
            .recover_signer()
            .expect("signed transaction should recover");

        assert_eq!(recovered.into_array(), expected.0);
    }
}
