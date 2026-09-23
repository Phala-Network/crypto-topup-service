//! The EVM JSON-RPC client shared by every consumer of one (chain, provider), and the
//! finalized-log reader built on it.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::{Future, IntoFuture};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::chain::flush::{
    ContractAddressGetter, DecodedFlushed, decode_address_of, decode_balance_of,
    decode_contract_address_getter, decode_flushed, encode_address_of, encode_balance_of,
    encode_contract_address_getter, flushed_signature,
};
use crate::redaction::{Redacted, RedactedTransportError};
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::{Provider, RootProvider};
use alloy::rpc::client::BatchRequest;
use alloy::rpc::types::{
    Filter, Log, Topic, TransactionInput, TransactionReceipt, TransactionRequest,
};
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::TransportError;
use chrono::{DateTime, Utc};
use serde_json::Value;
use tokio::time::timeout;
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

/// The provider's current finalized block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedHead {
    /// Finalized block number.
    pub number: u64,
    /// Timestamp of the finalized block.
    pub time: DateTime<Utc>,
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
    /// `eth_estimateGas` reported that execution reverts, a deterministic outcome.
    EstimationRevert(RedactedTransportError),
    /// The provider answered with a value that could not be encoded, decoded, or used.
    InvalidResponse(String),
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
            Self::Transport(error) | Self::EstimationRevert(error) => {
                Display::fmt(error, formatter)
            }
            Self::InvalidResponse(message) => formatter.write_str(message),
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

impl ChainError {
    /// Returns whether this error is a deterministic estimate execution revert.
    #[must_use]
    pub const fn is_estimation_revert(&self) -> bool {
        matches!(self, Self::EstimationRevert(_))
    }
}

/// One fee suggestion for an EIP-1559 transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeQuote {
    /// Maximum total fee per gas unit.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas unit.
    pub max_priority_fee_per_gas: u128,
}

/// Chain reads required by the scanner and confirm step.
pub trait ChainReader: Send + Sync {
    /// Returns the provider's current finalized block number and time.
    fn finalized_head(&self) -> impl Future<Output = Result<FinalizedHead, ChainError>> + Send;

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

/// Bounded FIFO block-timestamp cache keyed by block hash, so a reorged block at the same height
/// never lends its time to a log from another block. It evicts the oldest insertion, not the least
/// recently used entry: a hash's time never changes, and the head scan reads the newest blocks,
/// which are the newest insertions, so recency tracking would buy nothing.
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

/// Timeout for one bounded RPC request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Maximum calls in one JSON-RPC batch.
const BATCH_SIZE: usize = 500;

/// Alloy HTTP client for one RPC provider of one chain, shared by every consumer.
///
/// Errors carry the provider label, never the URL. Methods used by the flusher, reconciler,
/// refunds, sanctions screening and startup checks are bounded by the request timeout; the
/// finalized-log reads behind [`FinalizedReader`] are not, as the scanner and confirm step
/// bound them themselves.
pub struct EvmClient {
    provider: RootProvider,
    endpoint: Redacted,
    request_timeout: Duration,
}

impl fmt::Debug for EvmClient {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvmClient")
            .field("endpoint", &self.endpoint)
            .field("request_timeout", &self.request_timeout)
            .finish_non_exhaustive()
    }
}

impl EvmClient {
    /// Creates a client with the production request timeout.
    pub fn new(rpc_url: &str) -> Result<Self, ChainError> {
        Self::with_timeout(rpc_url, REQUEST_TIMEOUT)
    }

    /// Creates a client with an explicit request timeout, for tests.
    pub fn with_timeout(rpc_url: &str, request_timeout: Duration) -> Result<Self, ChainError> {
        let endpoint = Redacted::parse(rpc_url).map_err(|_| ChainError::InvalidUrl)?;
        Ok(Self {
            provider: RootProvider::new_http(endpoint.expose().clone()),
            endpoint,
            request_timeout,
        })
    }

    /// Labels provider errors with the configured provider id instead of the URL.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        self.endpoint = self.endpoint.with_provider(provider);
        self
    }

    /// Returns the redacted endpoint, for scheme checks and log labels.
    #[must_use]
    pub const fn endpoint(&self) -> &Redacted {
        &self.endpoint
    }

    /// Returns the bound applied to each request of the bounded methods.
    #[must_use]
    pub const fn request_timeout(&self) -> Duration {
        self.request_timeout
    }

    fn transport(&self, operation: &'static str, error: &TransportError) -> ChainError {
        ChainError::Transport(self.endpoint.rpc_error(operation, error))
    }

    /// Applies the request timeout, leaving the node's own answer to the caller.
    async fn within<T>(
        &self,
        operation: &'static str,
        request: impl IntoFuture<Output = Result<T, TransportError>>,
    ) -> Result<Result<T, TransportError>, ChainError> {
        timeout(self.request_timeout, request)
            .await
            .map_err(|_| ChainError::Transport(self.endpoint.timeout_error(operation)))
    }

    async fn bounded<T>(
        &self,
        operation: &'static str,
        request: impl IntoFuture<Output = Result<T, TransportError>>,
    ) -> Result<T, ChainError> {
        self.within(operation, request)
            .await?
            .map_err(|error| self.transport(operation, &error))
    }

    async fn batch_calls(
        &self,
        method: &'static str,
        params: Vec<Value>,
    ) -> Result<Vec<Value>, ChainError> {
        let mut batch = BatchRequest::new(self.provider.client());
        let mut waiters = Vec::with_capacity(params.len());
        for value in &params {
            waiters.push(
                batch
                    .add_call::<_, Value>(method, value)
                    .map_err(|error| self.transport("RPC batch construction", &error))?,
            );
        }
        self.bounded("RPC batch send", batch.send()).await?;
        let mut responses = Vec::with_capacity(waiters.len());
        for waiter in waiters {
            responses.push(self.bounded("RPC batch response", waiter).await?);
        }
        Ok(responses)
    }

    /// Runs one `eth_call` per item in bounded JSON-RPC batches and decodes each result.
    async fn batched_calls<I, T>(
        &self,
        items: &[I],
        call: impl Fn(&I) -> (Address, Bytes),
        block: BlockNumberOrTag,
        decode: impl Fn(&[u8]) -> Result<T, String>,
    ) -> Result<Vec<T>, ChainError> {
        let mut result = Vec::with_capacity(items.len());
        for chunk in items.chunks(BATCH_SIZE) {
            let params = chunk
                .iter()
                .map(|item| {
                    let (to, input) = call(item);
                    let tx = TransactionRequest::default()
                        .to(to)
                        .input(TransactionInput::new(input));
                    serde_json::to_value((tx, block)).map_err(|error| {
                        ChainError::InvalidResponse(format!("serialize eth_call: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            for value in self.batch_calls("eth_call", params).await? {
                let encoded: Bytes = serde_json::from_value(value).map_err(|error| {
                    ChainError::InvalidResponse(format!("decode eth_call bytes: {error}"))
                })?;
                result.push(decode(&encoded).map_err(ChainError::InvalidResponse)?);
            }
        }
        Ok(result)
    }

    /// Reads ERC-20 balances at one block in bounded JSON-RPC batches.
    pub async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
        block: BlockNumberOrTag,
    ) -> Result<Vec<U256>, ChainError> {
        self.batched_calls(
            addresses,
            |address| (token, encode_balance_of(*address)),
            block,
            |output| {
                decode_balance_of(output)
                    .map_err(|error| format!("decode balanceOf result: {error}"))
            },
        )
        .await
    }

    /// Reads latest native balances in bounded JSON-RPC batches.
    pub async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        let mut result = Vec::with_capacity(addresses.len());
        for chunk in addresses.chunks(BATCH_SIZE) {
            let params = chunk
                .iter()
                .map(|address| {
                    serde_json::to_value((*address, BlockNumberOrTag::Latest)).map_err(|error| {
                        ChainError::InvalidResponse(format!("serialize eth_getBalance: {error}"))
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            for value in self.batch_calls("eth_getBalance", params).await? {
                result.push(serde_json::from_value(value).map_err(|error| {
                    ChainError::InvalidResponse(format!("decode native balance: {error}"))
                })?);
            }
        }
        Ok(result)
    }

    /// Reads deterministic forwarder addresses at the latest block in bounded batches.
    pub async fn factory_addresses(
        &self,
        factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ChainError> {
        self.batched_calls(
            salts,
            |salt| (factory, encode_address_of(*salt)),
            BlockNumberOrTag::Latest,
            |output| {
                decode_address_of(output)
                    .map_err(|error| format!("decode addressOf result: {error}"))
            },
        )
        .await
    }

    /// Reads the runtime code deployed at `address`.
    pub async fn code_at(&self, address: Address) -> Result<Bytes, ChainError> {
        self.bounded("eth_getCode", self.provider.get_code_at(address))
            .await
    }

    /// Runs one `eth_call`, at `block` when given and otherwise at the node's default block.
    pub async fn call(
        &self,
        operation: &'static str,
        to: Address,
        input: Bytes,
        block: Option<BlockId>,
    ) -> Result<Bytes, ChainError> {
        let tx = TransactionRequest::default()
            .to(to)
            .input(TransactionInput::new(input));
        match block {
            Some(block) => {
                self.bounded(operation, self.provider.call(tx).block(block))
                    .await
            }
            None => self.bounded(operation, self.provider.call(tx)).await,
        }
    }

    /// Calls one immutable address getter of a forwarder contract.
    pub async fn contract_address(
        &self,
        contract: Address,
        getter: ContractAddressGetter,
    ) -> Result<Address, ChainError> {
        let output = self
            .call(
                "address getter call",
                contract,
                encode_contract_address_getter(getter),
                None,
            )
            .await?;
        decode_contract_address_getter(getter, &output).map_err(|error| {
            ChainError::InvalidResponse(format!("decode {getter:?} result: {error}"))
        })
    }

    /// Estimates gas for a call from `from`, distinguishing execution reverts.
    pub async fn estimate_gas(
        &self,
        from: Address,
        to: Address,
        input: Bytes,
    ) -> Result<u64, ChainError> {
        let tx = TransactionRequest::default()
            .from(from)
            .to(to)
            .input(TransactionInput::new(input));
        let operation = "eth_estimateGas";
        self.within(operation, self.provider.estimate_gas(tx))
            .await?
            .map_err(|error| {
                if error
                    .as_error_resp()
                    .is_some_and(|payload| is_execution_revert(&payload.message))
                {
                    ChainError::EstimationRevert(self.endpoint.rpc_error(operation, &error))
                } else {
                    self.transport(operation, &error)
                }
            })
    }

    /// Returns the account's nonce including pending transactions.
    pub async fn pending_nonce(&self, account: Address) -> Result<u64, ChainError> {
        self.bounded(
            "pending nonce",
            self.provider.get_transaction_count(account).pending(),
        )
        .await
    }

    /// Returns the account's nonce at the latest block.
    pub async fn confirmed_nonce(&self, account: Address) -> Result<u64, ChainError> {
        self.bounded(
            "confirmed nonce",
            self.provider.get_transaction_count(account).latest(),
        )
        .await
    }

    /// Returns the latest block number.
    pub async fn latest_block(&self) -> Result<u64, ChainError> {
        self.bounded("latest block", self.provider.get_block_number())
            .await
    }

    /// Returns the finalized block number, or `None` when the node has none.
    pub async fn finalized_block(&self) -> Result<Option<u64>, ChainError> {
        self.bounded(
            "finalized block",
            self.provider
                .get_block_number_by_id(BlockId::Number(BlockNumberOrTag::Finalized)),
        )
        .await
    }

    /// Returns an EIP-1559 fee suggestion.
    pub async fn fee_quote(&self) -> Result<FeeQuote, ChainError> {
        let estimate = self
            .bounded("fee estimate", self.provider.estimate_eip1559_fees())
            .await?;
        Ok(FeeQuote {
            max_fee_per_gas: estimate.max_fee_per_gas,
            max_priority_fee_per_gas: estimate.max_priority_fee_per_gas,
        })
    }

    /// Broadcasts a signed EIP-2718 transaction; a node that already knows it is success.
    pub async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError> {
        let operation = "send raw transaction";
        match self
            .within(operation, self.provider.send_raw_transaction(raw))
            .await?
        {
            Ok(pending) => Ok(*pending.tx_hash()),
            Err(error)
                if error
                    .as_error_resp()
                    .is_some_and(|payload| is_already_known(&payload.message)) =>
            {
                Ok(alloy::primitives::keccak256(raw))
            }
            Err(error) => Err(self.transport(operation, &error)),
        }
    }

    /// Reads a transaction receipt by hash.
    pub async fn receipt(&self, hash: B256) -> Result<Option<TransactionReceipt>, ChainError> {
        self.bounded(
            "transaction receipt",
            self.provider.get_transaction_receipt(hash),
        )
        .await
    }

    /// Reads one block with full transactions as raw JSON.
    pub async fn block_with_transactions(&self, number: u64) -> Result<Value, ChainError> {
        self.bounded(
            "block recovery",
            self.provider.raw_request(
                Cow::Borrowed("eth_getBlockByNumber"),
                (format!("0x{number:x}"), true),
            ),
        )
        .await
    }

    /// Returns the factory's `Flushed` events for `token` in one inclusive block window.
    pub async fn flushed_events(
        &self,
        factory: Address,
        token: Address,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<DecodedFlushed>, ChainError> {
        let filter = Filter::new()
            .address(factory)
            .from_block(from_block)
            .to_block(to_block)
            .event_signature(flushed_signature())
            .topic3(token);
        self.bounded("Flushed log fetch", self.provider.get_logs(&filter))
            .await?
            .iter()
            .map(|log| {
                decode_flushed(log.data())
                    .map_err(|error| ChainError::InvalidResponse(error.to_string()))
            })
            .collect()
    }

    /// Returns the provider's current `latest` block number. Used only by the display-only head
    /// scan; nothing that affects money reads above `finalized`.
    pub async fn latest_head(&self) -> Result<u64, ChainError> {
        self.provider
            .get_block_number()
            .await
            .map_err(|error| self.transport("latest head fetch", &error))
    }
}

fn is_already_known(message: &str) -> bool {
    let lowercase = message.to_ascii_lowercase();
    lowercase.contains("already known")
        || lowercase.contains("known transaction")
        || lowercase.contains("transaction already imported")
}

fn is_execution_revert(message: &str) -> bool {
    let lowercase = message.to_ascii_lowercase();
    lowercase.contains("execution reverted") || lowercase.contains("revert")
}

/// Finalized-log reader for one consumer of a shared [`EvmClient`].
///
/// Each consumer keeps its own finalized-head regression guard and block-time cache, so one
/// consumer's observations never change what another consumer reads.
pub struct FinalizedReader {
    client: Arc<EvmClient>,
    health: Mutex<ProviderHealth>,
    block_times: Mutex<BlockTimes>,
}

impl fmt::Debug for FinalizedReader {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalizedReader")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

impl FinalizedReader {
    /// Creates a reader with a fresh regression guard and block-time cache.
    #[must_use]
    pub fn new(client: Arc<EvmClient>) -> Self {
        Self {
            client,
            health: Mutex::new(ProviderHealth::default()),
            block_times: Mutex::new(BlockTimes::default()),
        }
    }

    /// Returns the shared client.
    #[must_use]
    pub const fn client(&self) -> &Arc<EvmClient> {
        &self.client
    }

    /// Returns the provider's current `latest` block number, for the display-only head scan.
    pub async fn latest_head(&self) -> Result<u64, ChainError> {
        self.client.latest_head().await
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
            .client
            .provider
            .get_block_by_hash(block_hash)
            .await
            .map_err(|error| self.client.transport("block timestamp fetch", &error))?
            .ok_or(ChainError::MissingField("block"))?;
        let time = utc_timestamp(block.header.inner.timestamp)?;
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
        let logs = self
            .client
            .provider
            .get_logs(&filter)
            .await
            .map_err(|error| self.client.transport("transfer log fetch", &error))?;
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

fn utc_timestamp(timestamp: u64) -> Result<DateTime<Utc>, ChainError> {
    i64::try_from(timestamp)
        .ok()
        .and_then(|value| DateTime::from_timestamp(value, 0))
        .ok_or(ChainError::InvalidTimestamp(timestamp))
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

impl ChainReader for FinalizedReader {
    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
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
            .client
            .provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await
            .map_err(|error| self.client.transport("finalized head fetch", &error))?
            .ok_or(ChainError::MissingField("finalized block"))?;
        let current = block.header.inner.number;
        let time = utc_timestamp(block.header.inner.timestamp)?;
        self.health
            .lock()
            .map_err(|_| ChainError::HealthStateUnavailable)?
            .observe(current)?;
        Ok(FinalizedHead {
            number: current,
            time,
        })
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
            .client
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|error| self.client.transport("transaction receipt fetch", &error))?
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

    #[test]
    fn configuration_never_echoes_an_invalid_url() {
        let secret = "not a url with api-key=secret";
        let error = EvmClient::new(secret).expect_err("invalid URL must fail");
        assert_eq!(error, ChainError::InvalidUrl);
        assert!(!error.to_string().contains(secret));
    }

    #[tokio::test]
    async fn node_rejection_reason_reaches_the_operator_without_the_url() {
        use axum::Router;
        use axum::http::header;
        use axum::routing::post;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let node = Router::new().route(
            "/rpc",
            post(|| async {
                (
                    [(header::CONTENT_TYPE, "application/json")],
                    r#"{"jsonrpc":"2.0","id":0,"error":{"code":-32000,"message":"nonce too low: next nonce 7, tx nonce 5"}}"#,
                )
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, node).await });
        let secret = "rpc-secret-token";
        let client = EvmClient::new(&format!(
            "http://user:{secret}@{address}/rpc?api_key={secret}"
        ))
        .expect("production adapter accepts URL")
        .with_provider("provider-a");

        let error = client
            .send_raw_transaction(&[0x02])
            .await
            .expect_err("node rejects the transaction");
        server.abort();

        let display = error.to_string();
        assert!(
            display.contains(
                "send raw transaction failed for provider `provider-a` \
                 (JSON-RPC error -32000: nonce too low: next nonce 7, tx nonce 5)"
            ),
            "{display}"
        );
        for rendered in [display, format!("{error:?} {client:?}")] {
            assert!(!rendered.contains(secret), "{rendered}");
            assert!(!rendered.contains("127.0.0.1"), "{rendered}");
        }
    }
}
