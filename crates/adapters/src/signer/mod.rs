//! Signing adapters.

use alloy_consensus::{SignableTransaction, TxEip1559};
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Bytes, TxKind};
use alloy_signer::SignerSync as _;
use alloy_signer_local::PrivateKeySigner;
use ed25519_dalek::{Signer as _, SigningKey};
use topup_core::{
    Ed25519PublicKey, Ed25519Signature, SecretKey32, SignedTx, SignerError, TxRequest,
};
use zeroize::Zeroizing;

pub mod actor;
pub mod dstack;

#[cfg(any(test, feature = "dev-signer"))]
mod dev;
#[cfg(feature = "dev-signer")]
pub use dev::DevSigner;

fn operator_signer(secret: &SecretKey32) -> Result<PrivateKeySigner, SignerError> {
    PrivateKeySigner::from_slice(secret.expose_secret()).map_err(|_| SignerError::InvalidKey)
}

fn sign_operator_tx(secret: &SecretKey32, request: TxRequest) -> Result<SignedTx, SignerError> {
    let signer = operator_signer(secret)?;
    let transaction = TxEip1559 {
        chain_id: request.chain_id,
        nonce: request.nonce,
        gas_limit: request.gas_limit,
        max_fee_per_gas: request.max_fee_per_gas,
        max_priority_fee_per_gas: request.max_priority_fee_per_gas,
        to: TxKind::Call(request.to),
        value: request.value,
        input: request.data,
        access_list: Default::default(),
    };
    let signature = signer
        .sign_hash_sync(&transaction.signature_hash())
        .map_err(|_| SignerError::SigningFailed)?;
    let signed = transaction.into_signed(signature);

    Ok(SignedTx {
        raw_signed_bytes: Bytes::from(signed.encoded_2718()),
    })
}

fn operator_address(secret: &SecretKey32) -> Result<alloy_primitives::Address, SignerError> {
    Ok(operator_signer(secret)?.address())
}

fn sign_settlement(secret: &SecretKey32, payload: &[u8]) -> Ed25519Signature {
    let signing_key = ed25519_signing_key(secret);
    Ed25519Signature(signing_key.sign(payload).to_bytes())
}

pub(crate) fn settlement_public_key(secret: &SecretKey32) -> Ed25519PublicKey {
    let signing_key = ed25519_signing_key(secret);
    Ed25519PublicKey(signing_key.verifying_key().to_bytes())
}

fn ed25519_signing_key(secret: &SecretKey32) -> SigningKey {
    let mut bytes = Zeroizing::new([0_u8; 32]);
    bytes.copy_from_slice(secret.expose_secret());
    SigningKey::from_bytes(&bytes)
}
