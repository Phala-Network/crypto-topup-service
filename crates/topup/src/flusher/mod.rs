//! Scheduled forwarder flush planning, sending, confirmation, and recovery.

mod chain;
mod engine;
mod planner;
mod sweep;
mod types;

use std::error::Error;
use std::fmt::{self, Display, Formatter};

use topup_core::SignerError;

pub use chain::AlloyChainClient;
pub use engine::{Flusher, RunResult};
pub use planner::{Planner, gas_ratio_allowed};
pub use sweep::SweepStep;
pub use types::{
    AlertSink, ChainClient, ChainError, ChainLog, ChainReceipt, FeeQuote, FlushAlert,
    FlushEvidence, FlusherPolicy, NoopAlertSink, PlannedAddress, PriceError, PriceSource,
    SignedVersion,
};

/// Failure while running a flusher operation.
#[derive(Debug)]
pub enum FlusherError {
    /// PostgreSQL operation failed.
    Database(sqlx::Error),
    /// Chain RPC operation failed.
    Chain(ChainError),
    /// Price lookup failed.
    Price(PriceError),
    /// Operator signing failed.
    Signer(SignerError),
    /// Stored JSON could not be encoded or decoded.
    Json(serde_json::Error),
    /// Stored evidence violated its expected shape.
    StoredEvidence(&'static str),
    /// Checked integer arithmetic exceeded its representation.
    Arithmetic,
    /// An internal or external invariant was violated.
    Invariant(&'static str),
}

impl Display for FlusherError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Database(error) => write!(formatter, "database operation failed: {error}"),
            Self::Chain(error) => write!(formatter, "chain operation failed: {error}"),
            Self::Price(error) => write!(formatter, "price lookup failed: {error}"),
            Self::Signer(error) => write!(formatter, "operator signing failed: {error}"),
            Self::Json(error) => write!(formatter, "flush evidence JSON failed: {error}"),
            Self::StoredEvidence(message) => write!(formatter, "stored flush evidence: {message}"),
            Self::Arithmetic => formatter.write_str("flush arithmetic is out of range"),
            Self::Invariant(message) => write!(formatter, "flusher invariant violated: {message}"),
        }
    }
}

impl Error for FlusherError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Chain(error) => Some(error),
            Self::Price(error) => Some(error),
            Self::Signer(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::StoredEvidence(_) | Self::Arithmetic | Self::Invariant(_) => None,
        }
    }
}

impl From<sqlx::Error> for FlusherError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<serde_json::Error> for FlusherError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
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
