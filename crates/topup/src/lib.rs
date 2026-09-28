//! Service boundary modules for the Phala Pay binary.

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
pub mod audit;
pub mod contracts;
pub mod db;
pub mod finality;
pub mod heartbeat;
pub mod ids;
pub mod jitter;
pub mod keys;
pub mod locks;
pub mod observability;
pub mod outbox;
mod pause;
pub mod pump;
pub mod reconciler;
pub mod refunds;
pub mod restore;
pub mod routes;
mod rpc_provider;
pub mod scanner;
pub mod steps;
pub mod tenancy;
