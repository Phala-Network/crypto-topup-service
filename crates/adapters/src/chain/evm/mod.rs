//! The EVM JSON-RPC client shared by every consumer of one (chain, provider), and the
//! finalized-log reader built on it.

use std::borrow::Cow;
use std::collections::{HashMap, VecDeque};
use std::fmt::{self, Formatter};
use std::future::{Future, IntoFuture};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::chain::flush::{
    ContractAddressGetter, DecodedFlushed, addressOfCall, balanceOfCall,
    decode_contract_address_getter, decode_flushed, encode_address_of, encode_balance_of,
    encode_contract_address_getter, flushed_signature,
};
use crate::redaction::{Redacted, RedactedTransportError};
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::bindings::IMulticall3::getEthBalanceCall;
use alloy::providers::{CallItem, MULTICALL3_ADDRESS, MulticallError, Provider, RootProvider};
use alloy::rpc::types::{
    Filter, Log, Topic, TransactionInput, TransactionReceipt, TransactionRequest,
};
use alloy::sol;
use alloy::sol_types::{SolCall, SolEvent};
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
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ChainError {
    /// The configured provider URL is invalid.
    #[error("invalid RPC URL")]
    InvalidUrl,
    /// The provider returned an RPC failure during the named operation.
    #[error("EVM RPC request failed during {0}")]
    Rpc(&'static str),
    /// The provider transport failed without exposing its configured URL.
    #[error("{0}")]
    Transport(RedactedTransportError),
    /// `eth_estimateGas` reported that execution reverts, a deterministic outcome.
    #[error("{0}")]
    EstimationRevert(RedactedTransportError),
    /// The provider answered with a value that could not be encoded, decoded, or used.
    #[error("{0}")]
    InvalidResponse(String),
    /// A required finalized block or log field was absent.
    #[error("EVM response omitted `{0}`")]
    MissingField(&'static str),
    /// A block timestamp did not fit the supported UTC representation.
    #[error("block timestamp `{0}` is outside UTC range")]
    InvalidTimestamp(u64),
    /// A log matching the transfer signature could not be decoded.
    #[error("invalid Transfer log: {0}")]
    InvalidTransfer(String),
    /// The caller supplied an invalid inclusive block range.
    #[error("invalid block range: from {from_block} exceeds to {to_block}")]
    InvalidRange {
        /// Inclusive range start.
        from_block: u64,
        /// Inclusive range end.
        to_block: u64,
    },
    /// The provider's finalized head moved backwards and it is now unhealthy.
    #[error("provider finalized head regressed from {previous} to {current}")]
    FinalizedHeadRegressed {
        /// Highest finalized head previously observed.
        previous: u64,
        /// Lower finalized head returned by the provider.
        current: u64,
    },
    /// A previous finalized-head regression permanently marked the provider unhealthy.
    #[error("provider is unhealthy after a finalized-head regression")]
    ProviderUnhealthy,
    /// The provider health lock was poisoned.
    #[error("provider health state unavailable")]
    HealthStateUnavailable,
}

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
/// Canonical Multicall3 deployment, through which every balance and `addressOf` read is
/// aggregated; `topup run` refuses a chain where it is missing or differs.
pub const MULTICALL3: Address = MULTICALL3_ADDRESS;
/// Calls aggregated into one Multicall3 `eth_call`, bounding its calldata (about 200 bytes per
/// call) and gas (a few thousand per view call) far below provider `eth_call` limits.
pub const MULTICALL_CHUNK: usize = 200;

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

    /// Aggregates same-typed view calls through Multicall3 `aggregate3` at `block`, one `eth_call`
    /// per [`MULTICALL_CHUNK`] calls, each with `allowFailure = false`, so one failing call fails
    /// the read instead of yielding a partial answer.
    ///
    /// These reads never use JSON-RPC batches or one request per item: public providers throttle
    /// batches far below their single-request limits (Tenderly's public Sepolia gateway refuses
    /// any batch of more than five `eth_call`s with `429 rate limit exceeded`, which stalled flush
    /// planning and failed reconciliation once a chain had six addresses), and one request per
    /// address grows with every address ever issued.
    async fn aggregate<D: SolCall + 'static>(
        &self,
        operation: &'static str,
        calls: Vec<CallItem<D>>,
        block: BlockId,
    ) -> Result<Vec<D::Return>, ChainError> {
        let mut results = Vec::with_capacity(calls.len());
        let mut calls = calls.into_iter().peekable();
        while calls.peek().is_some() {
            let multicall = self
                .provider
                .multicall()
                .dynamic::<D>()
                .extend_calls(calls.by_ref().take(MULTICALL_CHUNK))
                .block(block);
            let returns = timeout(self.request_timeout, multicall.aggregate3())
                .await
                .map_err(|_| ChainError::Transport(self.endpoint.timeout_error(operation)))?
                .map_err(|error| match error {
                    MulticallError::TransportError(error) => self.transport(operation, &error),
                    other => ChainError::InvalidResponse(format!("{operation}: {other}")),
                })?;
            for returned in returns {
                results.push(returned.map_err(|failure| {
                    ChainError::InvalidResponse(format!(
                        "{operation}: call {} returned undecodable data",
                        failure.idx
                    ))
                })?);
            }
        }
        Ok(results)
    }

    /// Reads ERC-20 balances at one block through Multicall3.
    pub async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
        block: BlockNumberOrTag,
    ) -> Result<Vec<U256>, ChainError> {
        let calls = addresses
            .iter()
            .map(|address| CallItem::<balanceOfCall>::new(token, encode_balance_of(*address)))
            .collect();
        self.aggregate("balanceOf multicall", calls, block.into())
            .await
    }

    /// Reads latest native balances through Multicall3 `getEthBalance`.
    pub async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError> {
        let calls = addresses
            .iter()
            .map(|address| {
                CallItem::<getEthBalanceCall>::new(
                    MULTICALL3,
                    getEthBalanceCall { addr: *address }.abi_encode().into(),
                )
            })
            .collect();
        self.aggregate("getEthBalance multicall", calls, BlockId::latest())
            .await
    }

    /// Reads deterministic forwarder addresses at the latest block through Multicall3.
    pub async fn factory_addresses(
        &self,
        factory: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ChainError> {
        let calls = salts
            .iter()
            .map(|salt| CallItem::<addressOfCall>::new(factory, encode_address_of(*salt)))
            .collect();
        self.aggregate("addressOf multicall", calls, BlockId::latest())
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
                    .is_some_and(|payload| is_execution_revert(payload.code, &payload.message))
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

/// geth, erigon, and Nethermind report a revert as JSON-RPC error 3; geth answers a revert without
/// return data with its default code and the bare `execution reverted` message. Anything else,
/// including an HTTP error whose JSON-RPC body merely mentions a revert, stays transient.
fn is_execution_revert(code: i64, message: &str) -> bool {
    code == 3
        || message
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("execution reverted")
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

    const SECRET: &str = "rpc-secret-token";

    /// Serves every JSON-RPC request with `status` and the JSON-RPC `error` object, behind a
    /// URL carrying `SECRET` in its credentials and query.
    async fn mock_node(
        status: u16,
        error: &'static str,
    ) -> (EvmClient, tokio::task::JoinHandle<std::io::Result<()>>) {
        use axum::Router;
        use axum::http::{StatusCode, header};
        use axum::routing::post;

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let status = StatusCode::from_u16(status).expect("valid status");
        let body = format!(r#"{{"jsonrpc":"2.0","id":0,"error":{error}}}"#);
        let node = Router::new().route(
            "/rpc",
            post(
                move || async move { (status, [(header::CONTENT_TYPE, "application/json")], body) },
            ),
        );
        let server = tokio::spawn(async move { axum::serve(listener, node).await });
        let client = EvmClient::new(&format!(
            "http://user:{SECRET}@{address}/rpc?api_key={SECRET}"
        ))
        .expect("production adapter accepts URL")
        .with_provider("provider-a");
        (client, server)
    }

    /// One `eth_call` the mock node answered: the block tag and each aggregated call's
    /// `allowFailure` flag.
    type ObservedCall = (Value, Vec<bool>);

    /// Answers like Tenderly's public Sepolia gateway (staging's provider A): a JSON-RPC batch
    /// carrying more than five `eth_call`s is refused with `429` and one `-32005` object, while
    /// single requests are served. A single `eth_call` must be Multicall3 `aggregate3`; every
    /// aggregated call returns the word `1`, and the node records what it answered.
    async fn batch_capped_node() -> (
        EvmClient,
        Arc<Mutex<Vec<ObservedCall>>>,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        use alloy::providers::bindings::IMulticall3::{Result as Call3Result, aggregate3Call};
        use axum::http::StatusCode;
        use axum::routing::post;
        use axum::{Json, Router};

        fn answer(request: &Value, observed: &Mutex<Vec<ObservedCall>>) -> Value {
            let transaction = &request["params"][0];
            let input = transaction
                .get("input")
                .or_else(|| transaction.get("data"))
                .and_then(Value::as_str)
                .and_then(|input| input.parse::<Bytes>().ok());
            let aggregate = input.and_then(|input| aggregate3Call::abi_decode(&input).ok());
            let (true, Some(aggregate)) = (
                transaction["to"].as_str().and_then(|to| to.parse().ok()) == Some(MULTICALL3),
                aggregate,
            ) else {
                return serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                    "code": -32000, "message": "only Multicall3 aggregate3 is served"}});
            };
            observed.lock().expect("observed calls").push((
                request["params"][1].clone(),
                aggregate
                    .calls
                    .iter()
                    .map(|call| call.allowFailure)
                    .collect(),
            ));
            let word = Bytes::from(U256::from(1).to_be_bytes::<32>());
            let results = aggregate
                .calls
                .iter()
                .map(|_| Call3Result {
                    success: true,
                    returnData: word.clone(),
                })
                .collect::<Vec<_>>();
            let output = Bytes::from(aggregate3Call::abi_encode_returns(&results));
            serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": output})
        }

        let observed = Arc::new(Mutex::new(Vec::new()));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let node = Router::new().route(
            "/rpc",
            post({
                let observed = Arc::clone(&observed);
                move |Json(body): Json<Value>| async move {
                    let Some(batch) = body.as_array() else {
                        return (StatusCode::OK, Json(answer(&body, &observed)));
                    };
                    let calls = batch
                        .iter()
                        .filter(|request| request["method"] == "eth_call")
                        .count();
                    if calls > 5 {
                        let refusal = serde_json::json!({"jsonrpc": "2.0", "id": 0, "error": {
                            "code": -32005, "message": "rate limit exceeded"}});
                        return (StatusCode::TOO_MANY_REQUESTS, Json(refusal));
                    }
                    let answers = batch
                        .iter()
                        .map(|request| answer(request, &observed))
                        .collect();
                    (StatusCode::OK, Json(answers))
                }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, node).await });
        let client = EvmClient::new(&format!("http://{address}/rpc"))
            .expect("production adapter accepts URL")
            .with_provider("provider-a");
        (client, observed, server)
    }

    #[tokio::test]
    async fn balance_and_address_reads_never_depend_on_provider_batch_limits() {
        let (client, observed, server) = batch_capped_node().await;
        let count = MULTICALL_CHUNK + 1;
        let addresses = (0..count)
            .map(|index| Address::with_last_byte(u8::try_from(index % 256).expect("byte")))
            .collect::<Vec<_>>();
        let salts = vec![B256::repeat_byte(1); 6];

        let tokens = client
            .token_balances(Address::ZERO, &addresses, BlockNumberOrTag::Number(7))
            .await;
        let natives = client.native_balances(&addresses[..6]).await;
        let derived = client.factory_addresses(Address::ZERO, &salts).await;
        server.abort();

        assert_eq!(tokens.expect("token balances"), vec![U256::from(1); count]);
        assert_eq!(natives.expect("native balances"), vec![U256::from(1); 6]);
        assert_eq!(
            derived.expect("derived addresses"),
            vec![Address::with_last_byte(1); 6]
        );
        // One aggregate3 `eth_call` per chunk, at the read's block, where no call may fail.
        let observed = observed.lock().expect("observed calls").clone();
        let shape = observed
            .iter()
            .map(|(block, calls)| (block.clone(), calls.len()))
            .collect::<Vec<_>>();
        assert_eq!(
            shape,
            vec![
                (Value::from("0x7"), MULTICALL_CHUNK),
                (Value::from("0x7"), 1),
                (Value::from("latest"), 6),
                (Value::from("latest"), 6),
            ]
        );
        assert!(
            observed
                .iter()
                .all(|(_, calls)| calls.iter().all(|allow| !allow))
        );
    }

    fn assert_redacted(error: &ChainError, client: &EvmClient) {
        for rendered in [error.to_string(), format!("{error:?} {client:?}")] {
            assert!(!rendered.contains(SECRET), "{rendered}");
            assert!(!rendered.contains("127.0.0.1"), "{rendered}");
        }
    }

    async fn estimate(status: u16, error: &'static str) -> ChainError {
        let (client, server) = mock_node(status, error).await;
        let result = client
            .estimate_gas(Address::ZERO, Address::ZERO, Bytes::new())
            .await;
        server.abort();
        let error = result.expect_err("node rejects the estimate");
        assert_redacted(&error, &client);
        error
    }

    #[tokio::test]
    async fn node_rejection_reason_reaches_the_operator_without_the_url() {
        let (client, server) = mock_node(
            200,
            r#"{"code":-32000,"message":"nonce too low: next nonce 7, tx nonce 5"}"#,
        )
        .await;

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
        assert_redacted(&error, &client);
    }

    // alloy 2 surfaces a JSON-RPC error body on a non-2xx response as that JSON-RPC error, not as
    // an HTTP error, so the classification must rest on the payload alone.
    #[tokio::test]
    async fn http_errors_with_a_json_rpc_body_are_transient_estimate_failures() {
        let error = estimate(
            502,
            r#"{"code":-32603,"message":"upstream failed while tracing a revert"}"#,
        )
        .await;
        assert!(matches!(error, ChainError::Transport(_)), "{error:?}");
        assert!(
            error.to_string().contains(
                "eth_estimateGas failed for provider `provider-a` \
                 (JSON-RPC error -32603: upstream failed while tracing a revert)"
            ),
            "{error}"
        );

        let error = estimate(
            429,
            r#"{"code":-32005,"message":"request rate exceeded; retry after revert window"}"#,
        )
        .await;
        assert!(matches!(error, ChainError::Transport(_)), "{error:?}");
        assert!(
            error.to_string().contains(
                "eth_estimateGas failed for provider `provider-a` \
                 (JSON-RPC error -32005: request rate exceeded; retry after revert window)"
            ),
            "{error}"
        );
    }

    #[tokio::test]
    async fn execution_reverts_are_deterministic_estimate_failures() {
        // geth, erigon, and Nethermind with revert data; geth without it.
        for body in [
            r#"{"code":3,"message":"execution reverted: nothing to flush","data":"0x08c379a0"}"#,
            r#"{"code":-32000,"message":"execution reverted"}"#,
        ] {
            let error = estimate(200, body).await;
            assert!(error.is_estimation_revert(), "{error:?}");
            assert!(
                error
                    .to_string()
                    .contains("eth_estimateGas failed for provider `provider-a` (JSON-RPC error"),
                "{error}"
            );
        }
    }
}
