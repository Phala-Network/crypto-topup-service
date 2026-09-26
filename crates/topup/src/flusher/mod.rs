//! Scheduled forwarder flush planning, sending, confirmation, and recovery.

mod chain;
mod engine;
mod planner;
pub mod runtime;
mod sweep;
mod types;

use topup_core::SignerError;

pub use engine::{Flusher, OperatorRole, RunResult};
pub use planner::{GasRatioInput, Planner, gas_ratio_allowed};
pub use sweep::SweepStep;
pub use topup_adapters::chain::evm::{ChainError, EvmClient, FeeQuote};
pub use topup_adapters::pricing::PriceError;
pub use types::{
    AlertSink, ChainClient, ChainLog, ChainReceipt, FlushAlert, FlushCallBinding, FlushEvidence,
    FlusherPolicy, NonceReceiptSearch, NoopAlertSink, PlannedAddress, PriceSource, SignedVersion,
};

/// Failure while running a flusher operation.
#[derive(Debug, thiserror::Error)]
pub enum FlusherError {
    /// PostgreSQL operation failed.
    #[error("database operation failed: {0}")]
    Database(#[from] sqlx::Error),
    /// Chain RPC operation failed.
    #[error("chain operation failed: {0}")]
    Chain(#[source] ChainError),
    /// Price lookup failed.
    #[error("price lookup failed: {0}")]
    Price(#[source] PriceError),
    /// Operator signing failed.
    #[error("operator signing failed: {0}")]
    Signer(#[source] SignerError),
    /// Stored JSON could not be encoded or decoded.
    #[error("flush evidence JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    /// Stored evidence violated its expected shape.
    #[error("stored flush evidence: {0}")]
    StoredEvidence(&'static str),
    /// Checked integer arithmetic exceeded its representation.
    #[error("flush arithmetic is out of range")]
    Arithmetic,
    /// An internal or external invariant was violated.
    #[error("flusher invariant violated: {0}")]
    Invariant(&'static str),
}

fn map_chain(error: ChainError) -> FlusherError {
    FlusherError::Chain(error)
}

fn map_price(error: PriceError) -> FlusherError {
    FlusherError::Price(error)
}

fn map_signer(error: SignerError) -> FlusherError {
    FlusherError::Signer(error)
}
