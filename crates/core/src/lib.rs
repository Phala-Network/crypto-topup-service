//! Pure domain types and rules for the crypto top-up service.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::expect_used,
        clippy::float_arithmetic,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )
)]

pub mod deposit;
pub mod retry;
mod signer;

pub use signer::{
    BACKUP_KEY_DOMAIN, Ed25519PublicKey, Ed25519Signature, EvmAddress, OPERATOR_KEY_DOMAIN,
    SETTLEMENT_KEY_DOMAIN, SecretKey32, SignedTx, Signer, SignerError, TxRequest,
};
