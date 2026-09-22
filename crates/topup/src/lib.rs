//! Service boundary modules for the crypto top-up binary.

#![cfg_attr(
    test,
    allow(
        clippy::arithmetic_side_effects,
        clippy::as_conversions,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::unwrap_used
    )
)]

pub mod db;
pub mod flusher;
pub mod outbox;
pub mod pump;
pub mod scanner;
