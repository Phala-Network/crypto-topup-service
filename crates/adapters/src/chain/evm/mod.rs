//! EVM finalized-log reader.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::sync::Mutex;

use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::{Filter, Topic};
use alloy::sol;
use alloy::sol_types::SolEvent;
use chrono::{DateTime, Utc};
use topup_core::money::AtomicAmount;
use url::Url;

/// Maximum inclusive block count in one `eth_getLogs` request.
pub const MAX_BLOCKS_PER_REQUEST: u64 = 2_000;
/// Maximum recipient count in one `eth_getLogs` request.
pub const MAX_ADDRESSES_PER_REQUEST: usize = 1_000;

sol! {
    event Transfer(address indexed from, address indexed to, uint256 amount);
}

/// One finalized ERC-20 transfer to a tracked address.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferLog {
    /// Transaction hash containing the event.
    pub tx_hash: B256,
    /// Log index within the block.
    pub log_index: u64,
    /// Finalized block number.
    pub block_number: u64,
    /// Finalized block hash.
    pub block_hash: B256,
    /// Timestamp of the finalized block.
    pub block_time: DateTime<Utc>,
    /// Token contract that emitted the event.
    pub token: Address,
    /// Transfer sender.
    pub from: Address,
    /// Transfer recipient.
    pub to: Address,
    /// Atomic token amount.
    pub amount: AtomicAmount,
}

/// Failure while reading or validating EVM chain data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ChainError {
    /// The configured provider URL is invalid.
    InvalidUrl(String),
    /// The provider returned an RPC failure during the named operation.
    Rpc(&'static str),
    /// A required finalized block or log field was absent.
    MissingField(&'static str),
    /// A block timestamp did not fit the supported UTC representation.
    InvalidTimestamp(u64),
    /// A log matching the transfer signature could not be decoded.
    InvalidTransfer(String),
    /// The caller supplied an invalid inclusive block range.
    InvalidRange {
        /// Inclusive range start.
        from_block: u64,
        /// Inclusive range end.
        to_block: u64,
    },
    /// The provider's finalized head moved backwards and it is now unhealthy.
    FinalizedHeadRegressed {
        /// Highest finalized head previously observed.
        previous: u64,
        /// Lower finalized head returned by the provider.
        current: u64,
    },
    /// A previous finalized-head regression permanently marked the provider unhealthy.
    ProviderUnhealthy,
    /// The provider health lock was poisoned.
    HealthStateUnavailable,
}

impl Display for ChainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(error) => write!(formatter, "invalid RPC URL: {error}"),
            Self::Rpc(operation) => {
                write!(formatter, "EVM RPC request failed during {operation}")
            }
            Self::MissingField(field) => write!(formatter, "EVM response omitted `{field}`"),
            Self::InvalidTimestamp(timestamp) => {
                write!(
                    formatter,
                    "block timestamp `{timestamp}` is outside UTC range"
                )
            }
            Self::InvalidTransfer(error) => write!(formatter, "invalid Transfer log: {error}"),
            Self::InvalidRange {
                from_block,
                to_block,
            } => write!(
                formatter,
                "invalid block range: from {from_block} exceeds to {to_block}"
            ),
            Self::FinalizedHeadRegressed { previous, current } => write!(
                formatter,
                "provider finalized head regressed from {previous} to {current}"
            ),
            Self::ProviderUnhealthy => {
                formatter.write_str("provider is unhealthy after a finalized-head regression")
            }
            Self::HealthStateUnavailable => {
                formatter.write_str("provider health state unavailable")
            }
        }
    }
}

impl Error for ChainError {}

/// Chain reads required by the scanner and confirm step.
pub trait ChainReader: Send + Sync {
    /// Returns the provider's current finalized block number.
    fn finalized_head(&self) -> impl Future<Output = Result<u64, ChainError>> + Send;

    /// Returns ERC-20 transfers to any supplied recipient in the inclusive block range.
    fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> impl Future<Output = Result<Vec<TransferLog>, ChainError>> + Send;
}

#[derive(Debug, Default)]
struct ProviderHealth {
    last_finalized: Option<u64>,
    unhealthy: bool,
}

impl ProviderHealth {
    fn observe(&mut self, current: u64) -> Result<(), ChainError> {
        if self.unhealthy {
            return Err(ChainError::ProviderUnhealthy);
        }
        if let Some(previous) = self.last_finalized
            && current < previous
        {
            self.unhealthy = true;
            return Err(ChainError::FinalizedHeadRegressed { previous, current });
        }
        self.last_finalized = Some(current);
        Ok(())
    }
}

/// Alloy HTTP client for finalized EVM reads.
#[derive(Debug)]
pub struct EvmChain {
    provider: RootProvider,
    health: Mutex<ProviderHealth>,
}

impl EvmChain {
    /// Creates a client for one RPC provider.
    pub fn new(rpc_url: &str) -> Result<Self, ChainError> {
        let url = Url::parse(rpc_url).map_err(|error| ChainError::InvalidUrl(error.to_string()))?;
        Ok(Self {
            provider: RootProvider::new_http(url),
            health: Mutex::new(ProviderHealth::default()),
        })
    }

    async fn block_time(&self, block_number: u64) -> Result<DateTime<Utc>, ChainError> {
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Number(block_number))
            .await
            .map_err(|_| ChainError::Rpc("block timestamp fetch"))?
            .ok_or(ChainError::MissingField("block"))?;
        let timestamp = block.header.inner.timestamp;
        let timestamp = i64::try_from(timestamp)
            .ok()
            .and_then(|value| DateTime::from_timestamp(value, 0))
            .ok_or(ChainError::InvalidTimestamp(timestamp))?;
        Ok(timestamp)
    }

    async fn transfer_logs_request(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
        timestamps: &mut BTreeMap<u64, DateTime<Utc>>,
    ) -> Result<Vec<TransferLog>, ChainError> {
        let recipients = addresses
            .iter()
            .copied()
            .fold(Topic::default(), Topic::extend);
        let filter = Filter::new()
            .from_block(from_block)
            .to_block(to_block)
            .event_signature(Transfer::SIGNATURE_HASH)
            .topic2(recipients);
        let logs = self
            .provider
            .get_logs(&filter)
            .await
            .map_err(|_| ChainError::Rpc("transfer log fetch"))?;
        let mut transfers = Vec::with_capacity(logs.len());
        for log in logs {
            let topic_count = log.topics().len();
            let data_length = log.data().data.len();
            if topic_count != 3 || data_length != 32 {
                tracing::warn!(
                    transaction_hash = ?log.transaction_hash,
                    log_index = ?log.log_index,
                    topic_count,
                    data_length,
                    "skipping Transfer log with a non-ERC-20 layout"
                );
                continue;
            }
            let decoded = match log.log_decode_validate::<Transfer>() {
                Ok(decoded) => decoded,
                Err(error) => {
                    tracing::warn!(
                        transaction_hash = ?log.transaction_hash,
                        log_index = ?log.log_index,
                        %error,
                        "skipping invalid ERC-20 Transfer log"
                    );
                    continue;
                }
            };
            let block_number = decoded
                .block_number
                .ok_or(ChainError::MissingField("log.block_number"))?;
            let block_time = match timestamps.get(&block_number) {
                Some(timestamp) => *timestamp,
                None => {
                    let timestamp = self.block_time(block_number).await?;
                    timestamps.insert(block_number, timestamp);
                    timestamp
                }
            };
            transfers.push(TransferLog {
                tx_hash: decoded
                    .transaction_hash
                    .ok_or(ChainError::MissingField("log.transaction_hash"))?,
                log_index: decoded
                    .log_index
                    .ok_or(ChainError::MissingField("log.log_index"))?,
                block_number,
                block_hash: decoded
                    .block_hash
                    .ok_or(ChainError::MissingField("log.block_hash"))?,
                block_time,
                token: decoded.address(),
                from: decoded.inner.data.from,
                to: decoded.inner.data.to,
                amount: AtomicAmount::new(decoded.inner.data.amount),
            });
        }
        Ok(transfers)
    }
}

impl ChainReader for EvmChain {
    async fn finalized_head(&self) -> Result<u64, ChainError> {
        {
            let health = self
                .health
                .lock()
                .map_err(|_| ChainError::HealthStateUnavailable)?;
            if health.unhealthy {
                return Err(ChainError::ProviderUnhealthy);
            }
        }
        let block = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await
            .map_err(|_| ChainError::Rpc("finalized head fetch"))?
            .ok_or(ChainError::MissingField("finalized block"))?;
        let current = block.header.inner.number;
        self.health
            .lock()
            .map_err(|_| ChainError::HealthStateUnavailable)?
            .observe(current)?;
        Ok(current)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        if from_block > to_block {
            return Err(ChainError::InvalidRange {
                from_block,
                to_block,
            });
        }
        if addresses.is_empty() {
            return Ok(Vec::new());
        }

        let mut transfers = Vec::new();
        for (window_from, window_to) in block_windows(from_block, to_block)? {
            let mut timestamps = BTreeMap::new();
            for batch in addresses.chunks(MAX_ADDRESSES_PER_REQUEST) {
                transfers.extend(
                    self.transfer_logs_request(batch, window_from, window_to, &mut timestamps)
                        .await?,
                );
            }
        }
        Ok(transfers)
    }
}

fn block_windows(from_block: u64, to_block: u64) -> Result<Vec<(u64, u64)>, ChainError> {
    if from_block > to_block {
        return Err(ChainError::InvalidRange {
            from_block,
            to_block,
        });
    }
    let mut windows = Vec::new();
    let mut start = from_block;
    loop {
        let end = start
            .saturating_add(MAX_BLOCKS_PER_REQUEST.saturating_sub(1))
            .min(to_block);
        windows.push((start, end));
        if end == to_block {
            break;
        }
        start = end.checked_add(1).ok_or(ChainError::InvalidRange {
            from_block,
            to_block,
        })?;
    }
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_inclusive_and_never_exceed_two_thousand_blocks() {
        assert_eq!(block_windows(7, 7).expect("valid range"), vec![(7, 7)]);
        assert_eq!(
            block_windows(10, 4_010).expect("valid range"),
            vec![(10, 2_009), (2_010, 4_009), (4_010, 4_010)]
        );
    }

    #[test]
    fn provider_is_permanently_unhealthy_after_finalized_head_regresses() {
        let mut health = ProviderHealth::default();
        assert_eq!(health.observe(100), Ok(()));
        assert_eq!(health.observe(101), Ok(()));
        assert_eq!(
            health.observe(99),
            Err(ChainError::FinalizedHeadRegressed {
                previous: 101,
                current: 99,
            })
        );
        assert_eq!(health.observe(102), Err(ChainError::ProviderUnhealthy));
    }

    #[test]
    fn address_chunks_never_exceed_one_thousand() {
        let addresses = vec![Address::ZERO; 2_001];
        let sizes = addresses
            .chunks(MAX_ADDRESSES_PER_REQUEST)
            .map(<[Address]>::len)
            .collect::<Vec<_>>();
        assert_eq!(sizes, vec![1_000, 1_000, 1]);
    }
}
