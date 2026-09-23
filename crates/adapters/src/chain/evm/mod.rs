//! EVM finalized-log reader.

use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::sync::{Mutex, PoisonError};

use crate::redaction::{Redacted, RedactedTransportError};
use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, B256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::types::{Filter, Log, Topic};
use alloy::sol;
use alloy::sol_types::SolEvent;
use chrono::{DateTime, Utc};
use topup_core::money::AtomicAmount;

/// Maximum inclusive block count in one `eth_getLogs` request.
pub const MAX_BLOCKS_PER_REQUEST: u64 = 2_000;
/// Maximum recipient count in one `eth_getLogs` request.
pub const MAX_ADDRESSES_PER_REQUEST: usize = 1_000;
/// Block timestamps kept per client; the head scan revisits the same recent blocks every poll.
const BLOCK_TIME_CACHE_CAPACITY: usize = 1_024;

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
    InvalidUrl,
    /// The provider returned an RPC failure during the named operation.
    Rpc(&'static str),
    /// The provider transport failed without exposing its configured URL.
    Transport(RedactedTransportError),
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
            Self::InvalidUrl => formatter.write_str("invalid RPC URL"),
            Self::Rpc(operation) => {
                write!(formatter, "EVM RPC request failed during {operation}")
            }
            Self::Transport(error) => Display::fmt(error, formatter),
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

    /// Locates one log by its immutable transaction hash and block-wide log index.
    fn transfer_log_by_identity(
        &self,
        tx_hash: B256,
        log_index: u64,
    ) -> impl Future<Output = Result<Option<TransferLog>, ChainError>> + Send;
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

/// Bounded block-timestamp cache keyed by block hash, so a reorged block at the same height never
/// lends its time to a log from another block. Evicts the oldest insertion when full.
#[derive(Debug, Default)]
struct BlockTimes {
    times: HashMap<B256, DateTime<Utc>>,
    order: VecDeque<B256>,
}

impl BlockTimes {
    fn get(&self, hash: &B256) -> Option<DateTime<Utc>> {
        self.times.get(hash).copied()
    }

    fn insert(&mut self, hash: B256, time: DateTime<Utc>) {
        if self.times.insert(hash, time).is_none() {
            self.order.push_back(hash);
            if self.order.len() > BLOCK_TIME_CACHE_CAPACITY
                && let Some(oldest) = self.order.pop_front()
            {
                self.times.remove(&oldest);
            }
        }
    }
}

/// Alloy HTTP client for finalized EVM reads.
pub struct EvmChain {
    provider: RootProvider,
    endpoint: Redacted,
    health: Mutex<ProviderHealth>,
    block_times: Mutex<BlockTimes>,
}

impl fmt::Debug for EvmChain {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvmChain")
            .field("endpoint", &self.endpoint)
            .finish_non_exhaustive()
    }
}

impl EvmChain {
    /// Creates a client for one RPC provider.
    pub fn new(rpc_url: &str) -> Result<Self, ChainError> {
        let endpoint = Redacted::parse(rpc_url).map_err(|_| ChainError::InvalidUrl)?;
        Ok(Self {
            provider: RootProvider::new_http(endpoint.expose().clone()),
            endpoint,
            health: Mutex::new(ProviderHealth::default()),
            block_times: Mutex::new(BlockTimes::default()),
        })
    }

    /// Labels provider errors with the configured provider id instead of the URL.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_provider(provider);
        self
    }

    /// Returns the provider's current `latest` block number. Used only by the display-only head
    /// scan; nothing that affects money reads above `finalized`.
    pub async fn latest_head(&self) -> Result<u64, ChainError> {
        self.provider.get_block_number().await.map_err(|error| {
            ChainError::Transport(self.endpoint.rpc_error("latest head fetch", &error))
        })
    }

    async fn block_time(&self, block_hash: B256) -> Result<DateTime<Utc>, ChainError> {
        let cached = self
            .block_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&block_hash);
        if let Some(time) = cached {
            return Ok(time);
        }
        let block = self
            .provider
            .get_block_by_hash(block_hash)
            .await
            .map_err(|error| {
                ChainError::Transport(self.endpoint.rpc_error("block timestamp fetch", &error))
            })?
            .ok_or(ChainError::MissingField("block"))?;
        let timestamp = block.header.inner.timestamp;
        let time = i64::try_from(timestamp)
            .ok()
            .and_then(|value| DateTime::from_timestamp(value, 0))
            .ok_or(ChainError::InvalidTimestamp(timestamp))?;
        self.block_times
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(block_hash, time);
        Ok(time)
    }

    async fn transfer_logs_request(
        &self,
        tokens: &[Address],
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        let recipients = addresses
            .iter()
            .copied()
            .fold(Topic::default(), Topic::extend);
        let mut filter = Filter::new()
            .from_block(from_block)
            .to_block(to_block)
            .event_signature(Transfer::SIGNATURE_HASH)
            .topic2(recipients);
        if !tokens.is_empty() {
            filter = filter.address(tokens.to_vec());
        }
        let logs = self.provider.get_logs(&filter).await.map_err(|error| {
            ChainError::Transport(self.endpoint.rpc_error("transfer log fetch", &error))
        })?;
        let mut transfers = Vec::with_capacity(logs.len());
        for log in logs {
            let block_hash = log
                .block_hash
                .ok_or(ChainError::MissingField("log.block_hash"))?;
            let block_time = self.block_time(block_hash).await?;
            if let Some(transfer) = decode_transfer_log(&log, block_time)? {
                transfers.push(transfer);
            }
        }
        Ok(transfers)
    }

    async fn transfer_logs(
        &self,
        tokens: &[Address],
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
            for batch in addresses.chunks(MAX_ADDRESSES_PER_REQUEST) {
                transfers.extend(
                    self.transfer_logs_request(tokens, batch, window_from, window_to)
                        .await?,
                );
            }
        }
        Ok(transfers)
    }

    /// Returns transfers of the given token contracts only; used by the display-only head scan
    /// so unsupported tokens cannot create pending rows or notifications.
    pub async fn token_transfer_logs_to(
        &self,
        tokens: &[Address],
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        if tokens.is_empty() {
            return Ok(Vec::new());
        }
        self.transfer_logs(tokens, addresses, from_block, to_block)
            .await
    }
}

fn decode_transfer_log(
    log: &Log,
    block_time: DateTime<Utc>,
) -> Result<Option<TransferLog>, ChainError> {
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
        return Ok(None);
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
            return Ok(None);
        }
    };
    Ok(Some(TransferLog {
        tx_hash: decoded
            .transaction_hash
            .ok_or(ChainError::MissingField("log.transaction_hash"))?,
        log_index: decoded
            .log_index
            .ok_or(ChainError::MissingField("log.log_index"))?,
        block_number: decoded
            .block_number
            .ok_or(ChainError::MissingField("log.block_number"))?,
        block_hash: decoded
            .block_hash
            .ok_or(ChainError::MissingField("log.block_hash"))?,
        block_time,
        token: decoded.address(),
        from: decoded.inner.data.from,
        to: decoded.inner.data.to,
        amount: AtomicAmount::new(decoded.inner.data.amount),
    }))
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
            .map_err(|error| {
                ChainError::Transport(self.endpoint.rpc_error("finalized head fetch", &error))
            })?
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
        self.transfer_logs(&[], addresses, from_block, to_block)
            .await
    }

    async fn transfer_log_by_identity(
        &self,
        tx_hash: B256,
        log_index: u64,
    ) -> Result<Option<TransferLog>, ChainError> {
        let Some(receipt) = self
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|error| {
                ChainError::Transport(self.endpoint.rpc_error("transaction receipt fetch", &error))
            })?
        else {
            return Ok(None);
        };
        let block_number = receipt
            .block_number
            .ok_or(ChainError::MissingField("receipt.block_number"))?;
        let block_hash = receipt
            .block_hash
            .ok_or(ChainError::MissingField("receipt.block_hash"))?;
        let Some(log) = receipt
            .logs()
            .iter()
            .find(|log| log.log_index == Some(log_index))
        else {
            return Ok(None);
        };
        if log.transaction_hash != Some(tx_hash)
            || log.block_number != Some(block_number)
            || log.block_hash != Some(block_hash)
        {
            return Err(ChainError::MissingField("receipt.log_identity"));
        }
        let block_time = self.block_time(block_hash).await?;
        decode_transfer_log(log, block_time)
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
    fn block_times_are_keyed_by_hash_and_bounded() {
        let mut times = BlockTimes::default();
        let time = |seconds| DateTime::from_timestamp(seconds, 0).expect("timestamp");
        let hash = |index: usize| B256::from(alloy::primitives::U256::from(index));
        for index in 0..=BLOCK_TIME_CACHE_CAPACITY {
            times.insert(hash(index), time(i64::try_from(index).expect("index")));
        }
        assert_eq!(times.times.len(), BLOCK_TIME_CACHE_CAPACITY);
        assert_eq!(times.get(&hash(0)), None, "oldest entry is evicted");
        assert_eq!(times.get(&hash(1)), Some(time(1)));
        // A different block at the same height has a different hash and never shares a time.
        assert_eq!(times.get(&hash(BLOCK_TIME_CACHE_CAPACITY + 1)), None);
    }

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
