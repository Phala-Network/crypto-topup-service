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

pub mod address;
pub mod deposit;
pub mod identity;
pub mod money;
pub mod retry;
pub mod route;
pub mod screening;
pub mod valuation;
