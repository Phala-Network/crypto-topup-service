//! External-system adapters for the crypto top-up service.

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

mod dev_feature_guard;

pub mod attestation;
pub mod chain;
pub mod pricing;
pub mod signer;
