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

pub mod api;
pub mod backup;
pub mod db;
pub mod flusher;
pub mod heartbeat;
pub mod locks;
pub mod outbox;
pub mod pump;
pub mod reconciler;
pub mod refunds;
pub mod restore;
mod rpc_provider;
pub mod scanner;
pub mod steps;
