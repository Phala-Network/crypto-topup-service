//! Reference-rate pricing adapters.

mod decimal;

pub mod binance;
pub mod coinmetrics;
pub mod kraken;

use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::Duration;

use async_trait::async_trait;

use crate::redaction::RedactedTransportError;

pub use topup_core::valuation::Observation;

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(8);

/// A timestamped USD price observation provider.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Fetches one current price observation.
    async fn observe(&self) -> Result<Observation, PriceError>;
}

/// Price adapter construction, transport, or response failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PriceError {
    /// The HTTP client could not be configured.
    ClientConfiguration,
    /// A configured endpoint was not a valid URL.
    InvalidUrl,
    /// The provider request failed or timed out.
    Request(RedactedTransportError),
    /// The provider returned a non-success status.
    HttpStatus(u16),
    /// The provider response did not match its documented schema.
    MalformedResponse(&'static str),
    /// A decimal price was invalid, zero, or outside the supported range.
    InvalidPrice,
    /// The observation timestamp was outside the supported Unix range.
    InvalidTimestamp,
    /// No source is configured for the requested asset identifier.
    UnconfiguredAsset(String),
}

impl Display for PriceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::ClientConfiguration => {
                formatter.write_str("price HTTP client configuration failed")
            }
            Self::InvalidUrl => formatter.write_str("price endpoint URL is invalid"),
            Self::Request(error) => Display::fmt(error, formatter),
            Self::HttpStatus(status) => write!(formatter, "price provider returned HTTP {status}"),
            Self::MalformedResponse(field) => {
                write!(formatter, "price provider response has invalid `{field}`")
            }
            Self::InvalidPrice => formatter.write_str("price provider returned an invalid price"),
            Self::InvalidTimestamp => {
                formatter.write_str("price provider returned an invalid timestamp")
            }
            Self::UnconfiguredAsset(asset) => write!(formatter, "no price source for `{asset}`"),
        }
    }
}

impl Error for PriceError {}

impl From<decimal::DecimalPriceError> for PriceError {
    fn from(_: decimal::DecimalPriceError) -> Self {
        Self::InvalidPrice
    }
}

fn http_client() -> Result<reqwest::Client, PriceError> {
    reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|_| PriceError::ClientConfiguration)
}

fn unix_now() -> Result<topup_core::valuation::UnixSeconds, PriceError> {
    let timestamp = chrono::Utc::now().timestamp();
    u64::try_from(timestamp)
        .map(topup_core::valuation::UnixSeconds::new)
        .map_err(|_| PriceError::InvalidTimestamp)
}
