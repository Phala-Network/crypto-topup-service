//! The EVM JSON-RPC client shared by every consumer of one (chain, provider), and the
//! finalized-log reader built on it.

pub mod metrics;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::fmt::{self, Formatter};
use std::future::{Future, IntoFuture};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use crate::chain::flush::{
    ContractAddressGetter, FactoryEvent, addressOfCall, balanceOfCall,
    decode_contract_address_getter, encode_address_of, encode_balance_of,
    encode_contract_address_getter, factory_event_signatures,
};
use crate::redaction::{Redacted, RedactedTransportError};
use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::{Address, B256, Bytes, U256};
use alloy::providers::{CallItem, MULTICALL3_ADDRESS, MulticallError, Provider, RootProvider};
use alloy::rpc::client::ClientBuilder;
use alloy::rpc::types::{
    Filter, Log, Topic, TransactionInput, TransactionReceipt, TransactionRequest,
};
use alloy::sol;
use alloy::sol_types::{SolCall, SolEvent};
use alloy::transports::TransportError;
use chrono::{DateTime, Utc};
use metrics::{CallLabels, CountingLayer};
use tokio::time::timeout;
use topup_core::money::AtomicAmount;
use topup_core::route::{ChainHeads, Confirmations};

/// Maximum inclusive block count in one `eth_getLogs` request.
pub const MAX_BLOCKS_PER_REQUEST: u64 = 2_000;
/// Maximum recipient count in one `eth_getLogs` request.
pub const MAX_ADDRESSES_PER_REQUEST: usize = 1_000;
/// Block timestamps kept per client; the head scan revisits the same recent blocks every poll.
const BLOCK_TIME_CACHE_CAPACITY: usize = 1_024;

sol! {
    event Transfer(address indexed from, address indexed to, uint256 amount);
    function isValidSignature(bytes32 hash, bytes signature) external view returns (bytes4);
}

/// The value EIP-1271's `isValidSignature` returns for a valid signature, its own selector.
pub const EIP1271_MAGIC_VALUE: [u8; 4] = [0x16, 0x26, 0xba, 0x7e];

/// One ERC-20 transfer to a tracked address.
///
/// Its identity is `(tx_hash, receipt_log_index)`, which survives the transaction's re-inclusion in
/// another block; the block fields and the block-wide `log_index` are evidence that may change.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransferLog {
    /// Transaction hash containing the event.
    pub tx_hash: B256,
    /// Position of the log among the logs of the transaction's receipt.
    pub receipt_log_index: u64,
    /// Log index within the block.
    pub log_index: u64,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: B256,
    /// Timestamp of the block.
    pub block_time: DateTime<Utc>,
    /// Sender of the transaction (not necessarily the token sender).
    pub tx_from: Address,
    /// Nonce of the transaction, which proves it dropped once another transaction consumed it.
    pub tx_nonce: u64,
    /// Token contract that emitted the event.
    pub token: Address,
    /// Transfer sender.
    pub from: Address,
    /// Transfer recipient.
    pub to: Address,
    /// Atomic token amount.
    pub amount: AtomicAmount,
}

/// A transaction's receipt as one provider reports it, with the transfer at one receipt position.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ReceiptLookup {
    /// The provider has no receipt: the transaction is not in its canonical chain.
    Missing,
    /// The transaction is included in `block_hash`.
    Included {
        /// Including block number.
        block_number: u64,
        /// Including block hash.
        block_hash: B256,
        /// The ERC-20 `Transfer` at the requested receipt position, if that log is one.
        transfer: Option<Box<TransferLog>>,
    },
}

impl ReceiptLookup {
    /// The transfer at the requested position, when the transaction is included.
    #[must_use]
    pub fn transfer(&self) -> Option<&TransferLog> {
        match self {
            Self::Missing => None,
            Self::Included { transfer, .. } => transfer.as_deref(),
        }
    }
}

/// What a recorded deposit already proves about its transfer, so re-reading it costs one receipt:
/// a block hash fixes the block's time, and a transaction hash fixes the transaction's nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KnownTransfer {
    /// Block hash the transfer was recorded in.
    pub block_hash: B256,
    /// Time of that block.
    pub block_time: DateTime<Utc>,
    /// Nonce of the transfer's transaction.
    pub tx_nonce: u64,
}

/// The provider's current finalized block.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FinalizedHead {
    /// Finalized block number.
    pub number: u64,
    /// Timestamp of the finalized block.
    pub time: DateTime<Utc>,
}

/// One `ForwarderFactory` event about a tracked forwarder.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FactoryLog {
    /// Transaction hash containing the event.
    pub tx_hash: B256,
    /// Log index within the block.
    pub log_index: u64,
    /// Block number.
    pub block_number: u64,
    /// Block hash.
    pub block_hash: B256,
    /// The decoded event.
    pub event: FactoryEvent,
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
    /// The provider answered a finalized head below one it answered before: a load-balanced
    /// gateway serving a node that has not caught up. The stale head is refused and the read is
    /// retried; the next answer at or above the highest is used.
    #[error("provider finalized head regressed from {previous} to {current}")]
    FinalizedHeadRegressed {
        /// Highest finalized head previously observed.
        previous: u64,
        /// Lower finalized head returned by the provider.
        current: u64,
    },
    /// The finalized-head guard lock was poisoned.
    #[error("provider health state unavailable")]
    HealthStateUnavailable,
    /// The chain moved between two reads that must describe the same block, such as a log and its
    /// transaction's receipt; the read is retried.
    #[error("chain reorganized during {0}")]
    Reorganized(&'static str),
}

impl ChainError {
    /// Returns whether the provider refused the request for now, so it may be retried after a
    /// backoff; see [`RedactedTransportError::is_rate_limited`].
    #[must_use]
    pub const fn is_rate_limited(&self) -> bool {
        matches!(self, Self::Transport(error) if error.is_rate_limited())
    }
}

/// Chain reads required by the scanner, the confirm step, and the finality watch.
pub trait ChainReader: Send + Sync {
    /// Returns the provider's current finalized block number and time.
    fn finalized_head(&self) -> impl Future<Output = Result<FinalizedHead, ChainError>> + Send;

    /// Returns the one head `confirmations` is evaluated on: `latest` for a depth, `safe` for
    /// `safe`, `finalized` for `finalized`. An unread `finalized` is 0, a lower bound, so a check
    /// on a depth or `safe` costs one head read.
    fn confirmation_heads(
        &self,
        confirmations: Confirmations,
    ) -> impl Future<Output = Result<ChainHeads, ChainError>> + Send;

    /// Returns the factory's `ForwarderCreated`, `Flushed`, and `FlushFailed` events about any
    /// supplied forwarder in the inclusive block range. Anyone can call the factory, so the caller
    /// decides which of them concern its own `(forwarder, treasury)` pairs.
    fn factory_logs(
        &self,
        factory: Address,
        forwarders: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> impl Future<Output = Result<Vec<FactoryLog>, ChainError>> + Send;

    /// Returns ERC-20 transfers to any supplied recipient in the inclusive block range.
    fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> impl Future<Output = Result<Vec<TransferLog>, ChainError>> + Send;

    /// Returns transfers of `tokens` to any address in `recipients` in the inclusive block range.
    ///
    /// A provider reader requests every transfer of the tokens, one request per block window
    /// whatever the recipient count, and keeps those to `recipients` locally.
    fn token_transfers(
        &self,
        tokens: &[Address],
        recipients: &BTreeSet<Address>,
        from_block: u64,
        to_block: u64,
    ) -> impl Future<Output = Result<Vec<TransferLog>, ChainError>> + Send {
        async move {
            let addresses = recipients.iter().copied().collect::<Vec<_>>();
            let logs = self
                .transfer_logs_to(&addresses, from_block, to_block)
                .await?;
            Ok(logs
                .into_iter()
                .filter(|log| tokens.contains(&log.token))
                .collect())
        }
    }

    /// Reads the transaction's receipt and the ERC-20 transfer at `receipt_log_index` in it.
    fn receipt_transfer(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
    ) -> impl Future<Output = Result<ReceiptLookup, ChainError>> + Send;

    /// [`Self::receipt_transfer`] for a recorded transfer: a provider reader reads only the
    /// receipt, taking the block time from `known` while the block hash is unchanged and the
    /// nonce from `known` always.
    fn receipt_transfer_known(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
        known: KnownTransfer,
    ) -> impl Future<Output = Result<ReceiptLookup, ChainError>> + Send {
        let _ = known;
        self.receipt_transfer(tx_hash, receipt_log_index)
    }

    /// Returns `account`'s nonce at block `block`: the number of its transactions included up to
    /// and including that block.
    fn nonce_at(
        &self,
        account: Address,
        block: u64,
    ) -> impl Future<Output = Result<u64, ChainError>> + Send;
}

/// The highest finalized head a reader has returned, so it never returns a lower one.
///
/// A load-balanced gateway can answer from nodes that disagree on `finalized` for minutes (Base
/// Sepolia's Tenderly gateway alternated between two heads 156 blocks apart), so a lower answer is
/// a stale node, not a finality violation: it is refused each time, and never marks the provider
/// unusable.
#[derive(Debug, Default)]
struct FinalizedGuard {
    last_finalized: Option<u64>,
}

impl FinalizedGuard {
    fn observe(&mut self, current: u64) -> Result<(), ChainError> {
        if let Some(previous) = self.last_finalized
            && current < previous
        {
            return Err(ChainError::FinalizedHeadRegressed { previous, current });
        }
        self.last_finalized = Some(current);
        Ok(())
    }
}

/// Bounded FIFO cache of values that never change for their key, such as a block's time by its
/// hash, so a reorged block at the same height never lends its data to a log from another block.
/// It evicts the oldest insertion, not the least recently used entry: a key's value never changes,
/// and the head scan reads the newest blocks, which are the newest insertions, so recency
/// tracking would buy nothing.
#[derive(Debug)]
struct FifoCache<K, V> {
    values: HashMap<K, V>,
    order: VecDeque<K>,
}

impl<K, V> Default for FifoCache<K, V> {
    fn default() -> Self {
        Self {
            values: HashMap::new(),
            order: VecDeque::new(),
        }
    }
}

impl<K: std::hash::Hash + Eq + Clone, V: Clone> FifoCache<K, V> {
    fn get(&self, key: &K) -> Option<V> {
        self.values.get(key).cloned()
    }

    fn insert(&mut self, key: K, value: V) {
        if self.values.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
            if self.order.len() > BLOCK_TIME_CACHE_CAPACITY
                && let Some(oldest) = self.order.pop_front()
            {
                self.values.remove(&oldest);
            }
        }
    }
}

type BlockTimes = FifoCache<B256, DateTime<Utc>>;

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
/// Errors carry the provider label, never the URL. Methods used by the reconciler, refunds,
/// sanctions screening and startup checks are bounded by the request timeout; the
/// finalized-log reads behind [`FinalizedReader`] are not, as the scanner and confirm step
/// bound them themselves.
pub struct EvmClient {
    provider: RootProvider,
    endpoint: Redacted,
    request_timeout: Duration,
    labels: CallLabels,
}

impl fmt::Debug for EvmClient {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EvmClient")
            .field("endpoint", &self.endpoint)
            .field("request_timeout", &self.request_timeout)
            .field("labels", &self.labels)
            .finish_non_exhaustive()
    }
}

/// An HTTP provider whose every request is counted under `labels` ([`metrics`]).
fn counted_provider(endpoint: &Redacted, labels: &CallLabels) -> RootProvider {
    RootProvider::new(
        ClientBuilder::default()
            .layer(CountingLayer::new(labels.clone()))
            .http(endpoint.expose().clone()),
    )
}

impl EvmClient {
    /// Creates a client with the production request timeout.
    pub fn new(rpc_url: &str) -> Result<Self, ChainError> {
        Self::with_timeout(rpc_url, REQUEST_TIMEOUT)
    }

    /// Creates a client with an explicit request timeout, for tests.
    pub fn with_timeout(rpc_url: &str, request_timeout: Duration) -> Result<Self, ChainError> {
        let endpoint = Redacted::parse(rpc_url).map_err(|_| ChainError::InvalidUrl)?;
        let labels = CallLabels::default();
        Ok(Self {
            provider: counted_provider(&endpoint, &labels),
            endpoint,
            request_timeout,
            labels,
        })
    }

    /// Labels provider errors and call counters with the configured provider id instead of the
    /// URL.
    #[must_use]
    pub fn with_provider(mut self, provider: impl Into<String>) -> Self {
        let provider = provider.into();
        self.labels.provider.clone_from(&provider);
        self.endpoint = self.endpoint.with_provider(provider);
        self.provider = counted_provider(&self.endpoint, &self.labels);
        self
    }

    /// Counts this client's calls under `chain_id`.
    #[must_use]
    pub fn with_chain_id(mut self, chain_id: u64) -> Self {
        self.labels.chain_id = Some(chain_id);
        self.provider = counted_provider(&self.endpoint, &self.labels);
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
    /// any batch of more than five `eth_call`s with `429 rate limit exceeded`, which failed
    /// reconciliation once a chain had six addresses), and one request per
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

    /// Reads the deterministic forwarder addresses of `treasury` at the latest block through
    /// Multicall3.
    pub async fn factory_addresses(
        &self,
        factory: Address,
        treasury: Address,
        salts: &[B256],
    ) -> Result<Vec<Address>, ChainError> {
        let calls = salts
            .iter()
            .map(|salt| CallItem::<addressOfCall>::new(factory, encode_address_of(treasury, *salt)))
            .collect();
        self.aggregate("addressOf multicall", calls, BlockId::latest())
            .await
    }

    /// Reads the runtime code deployed at `address`.
    pub async fn code_at(&self, address: Address) -> Result<Bytes, ChainError> {
        self.bounded("eth_getCode", self.provider.get_code_at(address))
            .await
    }

    /// Reads the runtime code at `address` as of block `block`.
    pub async fn code_at_block(&self, address: Address, block: u64) -> Result<Bytes, ChainError> {
        self.bounded(
            "eth_getCode",
            self.provider
                .get_code_at(address)
                .block_id(BlockId::number(block)),
        )
        .await
    }

    /// Asks the contract at `account`, as of block `block`, whether `signature` is its signature
    /// of `hash` (EIP-1271): `true` only when `isValidSignature(hash, signature)` returns the
    /// magic value `0x1626ba7e`. A revert or any other return value is `false`; a transport
    /// failure is an error.
    pub async fn is_valid_signature(
        &self,
        account: Address,
        hash: B256,
        signature: Bytes,
        block: u64,
    ) -> Result<bool, ChainError> {
        let operation = "isValidSignature call";
        let input = isValidSignatureCall { hash, signature }.abi_encode();
        let tx = TransactionRequest::default()
            .to(account)
            .input(TransactionInput::new(input.into()));
        let output = match self
            .within(
                operation,
                self.provider.call(tx).block(BlockId::number(block)),
            )
            .await?
        {
            Ok(output) => output,
            // The node executed the call and it reverted: the contract refuses the signature.
            Err(TransportError::ErrorResp(_)) => return Ok(false),
            Err(error) => return Err(self.transport(operation, &error)),
        };
        Ok(isValidSignatureCall::abi_decode_returns_validate(&output)
            .is_ok_and(|value| value.0 == EIP1271_MAGIC_VALUE))
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

    /// Returns the finalized block number, or `None` when the node has none.
    pub async fn finalized_block(&self) -> Result<Option<u64>, ChainError> {
        self.bounded(
            "finalized block",
            self.provider
                .get_block_number_by_id(BlockId::Number(BlockNumberOrTag::Finalized)),
        )
        .await
    }

    /// Reads a transaction receipt by hash.
    pub async fn receipt(&self, hash: B256) -> Result<Option<TransactionReceipt>, ChainError> {
        self.bounded(
            "transaction receipt",
            self.provider.get_transaction_receipt(hash),
        )
        .await
    }

    /// Reads a transaction's sender and nonce by hash, pending or included; `None` when the
    /// provider does not know the transaction.
    pub async fn transaction_origin(
        &self,
        hash: B256,
    ) -> Result<Option<(Address, u64)>, ChainError> {
        use alloy::consensus::Transaction as _;
        use alloy::network::TransactionResponse as _;

        let transaction = self
            .bounded("transaction", self.provider.get_transaction_by_hash(hash))
            .await?;
        Ok(transaction.map(|transaction| (transaction.from(), transaction.nonce())))
    }

    /// Returns `account`'s nonce at `block`: the number of its transactions up to that block.
    pub async fn nonce_at(&self, account: Address, block: u64) -> Result<u64, ChainError> {
        self.bounded(
            "nonce",
            self.provider
                .get_transaction_count(account)
                .block_id(BlockId::number(block)),
        )
        .await
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

/// Chain-log reader for one consumer of a shared [`EvmClient`].
///
/// Each consumer keeps its own finalized-head regression guard and caches, so one consumer's
/// observations never change what another consumer reads. Every transfer it returns carries its
/// receipt position and its transaction's sender and nonce, read once per transaction.
pub struct FinalizedReader {
    client: Arc<EvmClient>,
    finalized_guard: Mutex<FinalizedGuard>,
    block_times: Mutex<BlockTimes>,
    /// Block-wide log indexes of a receipt's logs, in receipt order, by `(tx_hash, block_hash)`.
    receipt_logs: Mutex<FifoCache<(B256, B256), Vec<u64>>>,
    /// A transaction's sender and nonce, which its hash commits to.
    origins: Mutex<FifoCache<B256, (Address, u64)>>,
}

impl fmt::Debug for FinalizedReader {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FinalizedReader")
            .field("client", &self.client)
            .finish_non_exhaustive()
    }
}

/// How a transfer-log request selects recipients.
#[derive(Clone, Copy)]
enum Recipients<'a> {
    /// In the request's recipient topic.
    Topic(&'a [Address]),
    /// Kept locally from every transfer of the requested tokens.
    Local(&'a BTreeSet<Address>),
}

/// A decoded `Transfer` log before its receipt position and transaction origin are known.
struct DecodedTransfer {
    tx_hash: B256,
    log_index: u64,
    block_number: u64,
    block_hash: B256,
    token: Address,
    from: Address,
    to: Address,
    amount: AtomicAmount,
}

impl DecodedTransfer {
    fn complete(
        self,
        receipt_log_index: u64,
        block_time: DateTime<Utc>,
        (tx_from, tx_nonce): (Address, u64),
    ) -> TransferLog {
        TransferLog {
            tx_hash: self.tx_hash,
            receipt_log_index,
            log_index: self.log_index,
            block_number: self.block_number,
            block_hash: self.block_hash,
            block_time,
            tx_from,
            tx_nonce,
            token: self.token,
            from: self.from,
            to: self.to,
            amount: self.amount,
        }
    }
}

impl FinalizedReader {
    /// Creates a reader with a fresh regression guard and caches.
    #[must_use]
    pub fn new(client: Arc<EvmClient>) -> Self {
        Self {
            client,
            finalized_guard: Mutex::new(FinalizedGuard::default()),
            block_times: Mutex::new(BlockTimes::default()),
            receipt_logs: Mutex::new(FifoCache::default()),
            origins: Mutex::new(FifoCache::default()),
        }
    }

    /// Returns the shared client.
    #[must_use]
    pub const fn client(&self) -> &Arc<EvmClient> {
        &self.client
    }

    /// Returns the provider's current `latest` block number (`eth_blockNumber`).
    pub async fn latest_head(&self) -> Result<u64, ChainError> {
        self.client.latest_head().await
    }

    /// Returns the provider's current `safe` block number.
    pub async fn safe_head(&self) -> Result<u64, ChainError> {
        self.tagged_block_number(BlockNumberOrTag::Safe, "safe head fetch")
            .await
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

    async fn receipt(&self, tx_hash: B256) -> Result<Option<TransactionReceipt>, ChainError> {
        self.client
            .provider
            .get_transaction_receipt(tx_hash)
            .await
            .map_err(|error| self.client.transport("transaction receipt fetch", &error))
    }

    /// The transaction's sender and nonce. A transaction the provider no longer knows was
    /// reorganized away between the reads.
    async fn origin(&self, tx_hash: B256) -> Result<(Address, u64), ChainError> {
        use alloy::consensus::Transaction as _;
        use alloy::network::TransactionResponse as _;

        let cached = self
            .origins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&tx_hash);
        if let Some(origin) = cached {
            return Ok(origin);
        }
        let transaction = self
            .client
            .provider
            .get_transaction_by_hash(tx_hash)
            .await
            .map_err(|error| self.client.transport("transaction fetch", &error))?
            .ok_or(ChainError::Reorganized("transaction fetch"))?;
        let origin = (transaction.from(), transaction.nonce());
        self.origins
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(tx_hash, origin);
        Ok(origin)
    }

    /// The position of the log with block-wide index `log_index` in its transaction's receipt,
    /// which must be in `block_hash`.
    async fn receipt_position(
        &self,
        tx_hash: B256,
        block_hash: B256,
        log_index: u64,
    ) -> Result<u64, ChainError> {
        let cached = self
            .receipt_logs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(tx_hash, block_hash));
        let indexes = match cached {
            Some(indexes) => indexes,
            None => {
                let receipt = self
                    .receipt(tx_hash)
                    .await?
                    .ok_or(ChainError::Reorganized("receipt fetch"))?;
                if receipt.block_hash != Some(block_hash) {
                    return Err(ChainError::Reorganized("receipt fetch"));
                }
                let indexes = receipt
                    .logs()
                    .iter()
                    .map(|log| {
                        log.log_index
                            .ok_or(ChainError::MissingField("log.log_index"))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                self.receipt_logs
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert((tx_hash, block_hash), indexes.clone());
                indexes
            }
        };
        let position = indexes
            .iter()
            .position(|index| *index == log_index)
            .ok_or(ChainError::MissingField("receipt log"))?;
        u64::try_from(position).map_err(|_| ChainError::MissingField("receipt log position"))
    }

    async fn complete(&self, decoded: DecodedTransfer) -> Result<TransferLog, ChainError> {
        let block_time = self.block_time(decoded.block_hash).await?;
        let position = self
            .receipt_position(decoded.tx_hash, decoded.block_hash, decoded.log_index)
            .await?;
        let origin = self.origin(decoded.tx_hash).await?;
        Ok(decoded.complete(position, block_time, origin))
    }

    async fn transfer_logs_request(
        &self,
        tokens: &[Address],
        recipients: Recipients<'_>,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        let mut filter = Filter::new()
            .from_block(from_block)
            .to_block(to_block)
            .event_signature(Transfer::SIGNATURE_HASH);
        if let Recipients::Topic(addresses) = recipients {
            filter = filter.topic2(
                addresses
                    .iter()
                    .copied()
                    .fold(Topic::default(), Topic::extend),
            );
        }
        if !tokens.is_empty() {
            filter = filter.address(tokens.to_vec());
        }
        let logs = self
            .client
            .provider
            .get_logs(&filter)
            .await
            .map_err(|error| self.client.transport("transfer log fetch", &error))?;
        let mut transfers = Vec::new();
        for log in logs {
            let Some(decoded) = decode_transfer_log(&log)? else {
                continue;
            };
            if let Recipients::Local(kept) = recipients
                && !kept.contains(&decoded.to)
            {
                continue;
            }
            // Nodes that report the block time with the log spare one block read per block.
            if let Some(timestamp) = log.block_timestamp {
                let time = utc_timestamp(timestamp)?;
                self.block_times
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .insert(decoded.block_hash, time);
            }
            transfers.push(self.complete(decoded).await?);
        }
        Ok(transfers)
    }

    async fn transfer_logs(
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
            for batch in addresses.chunks(MAX_ADDRESSES_PER_REQUEST) {
                transfers.extend(
                    self.transfer_logs_request(
                        &[],
                        Recipients::Topic(batch),
                        window_from,
                        window_to,
                    )
                    .await?,
                );
            }
        }
        Ok(transfers)
    }

    /// Every factory event in the range, one request whatever the forwarder count, kept when it
    /// is about one of `forwarders`.
    async fn factory_logs_request(
        &self,
        factory: Address,
        forwarders: &BTreeSet<Address>,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<FactoryLog>, ChainError> {
        let filter = Filter::new()
            .address(factory)
            .from_block(from_block)
            .to_block(to_block)
            .event_signature(factory_event_signatures().to_vec());
        let logs = self
            .client
            .provider
            .get_logs(&filter)
            .await
            .map_err(|error| self.client.transport("factory log fetch", &error))?;
        let mut kept = Vec::new();
        for log in &logs {
            let decoded = decode_factory_log(log)?;
            if forwarders.contains(&decoded.event.forwarder()) {
                kept.push(decoded);
            }
        }
        Ok(kept)
    }

    async fn tagged_block_number(
        &self,
        tag: BlockNumberOrTag,
        operation: &'static str,
    ) -> Result<u64, ChainError> {
        Ok(self
            .client
            .provider
            .get_block_by_number(tag)
            .await
            .map_err(|error| self.client.transport(operation, &error))?
            .ok_or(ChainError::MissingField("tagged block"))?
            .header
            .inner
            .number)
    }
}

fn utc_timestamp(timestamp: u64) -> Result<DateTime<Utc>, ChainError> {
    i64::try_from(timestamp)
        .ok()
        .and_then(|value| DateTime::from_timestamp(value, 0))
        .ok_or(ChainError::InvalidTimestamp(timestamp))
}

fn decode_transfer_log(log: &Log) -> Result<Option<DecodedTransfer>, ChainError> {
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
    Ok(Some(DecodedTransfer {
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
        token: decoded.address(),
        from: decoded.inner.data.from,
        to: decoded.inner.data.to,
        amount: AtomicAmount::new(decoded.inner.data.amount),
    }))
}

fn decode_factory_log(log: &Log) -> Result<FactoryLog, ChainError> {
    let event = FactoryEvent::decode(log.data())
        .map_err(|error| ChainError::InvalidResponse(format!("factory event: {error}")))?;
    Ok(FactoryLog {
        tx_hash: log
            .transaction_hash
            .ok_or(ChainError::MissingField("log.transaction_hash"))?,
        log_index: log
            .log_index
            .ok_or(ChainError::MissingField("log.log_index"))?,
        block_number: log
            .block_number
            .ok_or(ChainError::MissingField("log.block_number"))?,
        block_hash: log
            .block_hash
            .ok_or(ChainError::MissingField("log.block_hash"))?,
        event,
    })
}

/// Whether `log` is an ERC-20 `Transfer` event: the signature topic and the ERC-20 layout.
fn is_transfer(log: &Log) -> bool {
    log.topics().first() == Some(&Transfer::SIGNATURE_HASH)
}

impl ChainReader for FinalizedReader {
    async fn finalized_head(&self) -> Result<FinalizedHead, ChainError> {
        let block = self
            .client
            .provider
            .get_block_by_number(BlockNumberOrTag::Finalized)
            .await
            .map_err(|error| self.client.transport("finalized head fetch", &error))?
            .ok_or(ChainError::MissingField("finalized block"))?;
        let current = block.header.inner.number;
        let time = utc_timestamp(block.header.inner.timestamp)?;
        self.finalized_guard
            .lock()
            .map_err(|_| ChainError::HealthStateUnavailable)?
            .observe(current)?;
        Ok(FinalizedHead {
            number: current,
            time,
        })
    }

    async fn confirmation_heads(
        &self,
        confirmations: Confirmations,
    ) -> Result<ChainHeads, ChainError> {
        let mut heads = ChainHeads::default();
        if confirmations.needs_latest() {
            heads.latest = Some(self.client.latest_head().await?);
        } else if confirmations.needs_safe() {
            heads.safe = Some(self.safe_head().await?);
        } else {
            heads.finalized = ChainReader::finalized_head(self).await?.number;
        }
        Ok(heads)
    }

    async fn transfer_logs_to(
        &self,
        addresses: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        self.transfer_logs(addresses, from_block, to_block).await
    }

    async fn token_transfers(
        &self,
        tokens: &[Address],
        recipients: &BTreeSet<Address>,
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<TransferLog>, ChainError> {
        if from_block > to_block {
            return Err(ChainError::InvalidRange {
                from_block,
                to_block,
            });
        }
        let mut transfers = Vec::new();
        if tokens.is_empty() || recipients.is_empty() {
            return Ok(transfers);
        }
        for (window_from, window_to) in block_windows(from_block, to_block)? {
            transfers.extend(
                self.transfer_logs_request(
                    tokens,
                    Recipients::Local(recipients),
                    window_from,
                    window_to,
                )
                .await?,
            );
        }
        Ok(transfers)
    }

    async fn factory_logs(
        &self,
        factory: Address,
        forwarders: &[Address],
        from_block: u64,
        to_block: u64,
    ) -> Result<Vec<FactoryLog>, ChainError> {
        if from_block > to_block {
            return Err(ChainError::InvalidRange {
                from_block,
                to_block,
            });
        }
        let mut logs = Vec::new();
        if forwarders.is_empty() {
            return Ok(logs);
        }
        let forwarders = forwarders.iter().copied().collect::<BTreeSet<_>>();
        for (window_from, window_to) in block_windows(from_block, to_block)? {
            logs.extend(
                self.factory_logs_request(factory, &forwarders, window_from, window_to)
                    .await?,
            );
        }
        Ok(logs)
    }

    async fn receipt_transfer(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
    ) -> Result<ReceiptLookup, ChainError> {
        self.receipt_lookup(tx_hash, receipt_log_index, None).await
    }

    async fn receipt_transfer_known(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
        known: KnownTransfer,
    ) -> Result<ReceiptLookup, ChainError> {
        self.receipt_lookup(tx_hash, receipt_log_index, Some(known))
            .await
    }

    async fn nonce_at(&self, account: Address, block: u64) -> Result<u64, ChainError> {
        self.client
            .provider
            .get_transaction_count(account)
            .block_id(BlockId::number(block))
            .await
            .map_err(|error| self.client.transport("nonce fetch", &error))
    }
}

impl FinalizedReader {
    async fn receipt_lookup(
        &self,
        tx_hash: B256,
        receipt_log_index: u64,
        known: Option<KnownTransfer>,
    ) -> Result<ReceiptLookup, ChainError> {
        let Some(receipt) = self.receipt(tx_hash).await? else {
            return Ok(ReceiptLookup::Missing);
        };
        let block_number = receipt
            .block_number
            .ok_or(ChainError::MissingField("receipt.block_number"))?;
        let block_hash = receipt
            .block_hash
            .ok_or(ChainError::MissingField("receipt.block_hash"))?;
        let log = usize::try_from(receipt_log_index)
            .ok()
            .and_then(|position| receipt.logs().get(position));
        let decoded = match log {
            Some(log) if is_transfer(log) => {
                if log.transaction_hash != Some(tx_hash)
                    || log.block_number != Some(block_number)
                    || log.block_hash != Some(block_hash)
                {
                    return Err(ChainError::MissingField("receipt.log_identity"));
                }
                decode_transfer_log(log)?
            }
            _ => None,
        };
        let transfer = match decoded {
            Some(decoded) => {
                let block_time = match known {
                    Some(known) if known.block_hash == block_hash => known.block_time,
                    _ => self.block_time(block_hash).await?,
                };
                let nonce = match known {
                    Some(known) => known.tx_nonce,
                    None => self.origin(tx_hash).await?.1,
                };
                let origin = (receipt.from, nonce);
                Some(Box::new(decoded.complete(
                    receipt_log_index,
                    block_time,
                    origin,
                )))
            }
            None => None,
        };
        Ok(ReceiptLookup::Included {
            block_number,
            block_hash,
            transfer,
        })
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
    use alloy::primitives::{address, b256};
    use serde_json::Value;

    use super::*;

    #[test]
    fn block_times_are_keyed_by_hash_and_bounded() {
        let mut times = BlockTimes::default();
        let time = |seconds| DateTime::from_timestamp(seconds, 0).expect("timestamp");
        let hash = |index: usize| B256::from(alloy::primitives::U256::from(index));
        for index in 0..=BLOCK_TIME_CACHE_CAPACITY {
            times.insert(hash(index), time(i64::try_from(index).expect("index")));
        }
        assert_eq!(times.values.len(), BLOCK_TIME_CACHE_CAPACITY);
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

    /// Answers recorded from Base Sepolia's provider A (the Tenderly gateway) on 2026-09-29.
    fn base_sepolia(name: &str) -> Value {
        let json = match name {
            "block-47445875" => {
                include_str!("../../../tests/fixtures/base-sepolia/block-47445875.json")
            }
            "finalized-47446540" => {
                include_str!("../../../tests/fixtures/base-sepolia/finalized-47446540.json")
            }
            "finalized-47446696" => {
                include_str!("../../../tests/fixtures/base-sepolia/finalized-47446696.json")
            }
            "receipt" => include_str!("../../../tests/fixtures/base-sepolia/receipt.json"),
            "transaction" => include_str!("../../../tests/fixtures/base-sepolia/transaction.json"),
            "transfer-logs" => {
                include_str!("../../../tests/fixtures/base-sepolia/transfer-logs.json")
            }
            other => panic!("no fixture {other}"),
        };
        serde_json::from_str(json).expect("fixture is JSON")
    }

    /// Serves each JSON-RPC method its recorded answer, and the `finalized` block reads the
    /// recorded heads in turn.
    async fn replay_node(
        answers: Vec<(&'static str, Value)>,
        finalized: Vec<Value>,
    ) -> (
        FinalizedReader,
        tokio::task::JoinHandle<std::io::Result<()>>,
    ) {
        use axum::routing::post;
        use axum::{Json, Router};

        let answers = Arc::new(answers.into_iter().collect::<HashMap<_, _>>());
        let finalized = Arc::new(Mutex::new(VecDeque::from(finalized)));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local listener binds");
        let address = listener.local_addr().expect("listener address");
        let node = Router::new().route(
            "/rpc",
            post(move |Json(request): Json<Value>| async move {
                let method = request["method"].as_str().unwrap_or_default();
                let result =
                    if method == "eth_getBlockByNumber" && request["params"][0] == "finalized" {
                        finalized.lock().expect("finalized answers").pop_front()
                    } else {
                        answers.get(method).cloned()
                    };
                Json(match result {
                    Some(result) => {
                        serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "result": result})
                    }
                    None => serde_json::json!({"jsonrpc": "2.0", "id": request["id"], "error": {
                        "code": -32601, "message": format!("{method} not recorded")}}),
                })
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, node).await });
        let client = EvmClient::new(&format!("http://{address}/rpc"))
            .expect("production adapter accepts URL")
            .with_provider("base-sepolia-a");
        (FinalizedReader::new(Arc::new(client)), server)
    }

    // On 2026-09-29 the gateway answered `finalized` 47446696, then 47446540 for minutes, while
    // provider B answered 47446696 with the same hash: a lagging node, not a finality violation.
    // Marking the provider unusable for good stopped Base Sepolia's scanner.
    #[tokio::test]
    async fn a_gateway_flapping_between_finalized_heads_is_refused_then_used_again() {
        let (reader, server) = replay_node(
            Vec::new(),
            vec![
                base_sepolia("finalized-47446696"),
                base_sepolia("finalized-47446540"),
                base_sepolia("finalized-47446540"),
                base_sepolia("finalized-47446696"),
            ],
        )
        .await;
        let mut heads = Vec::new();
        for _ in 0..4 {
            heads.push(reader.finalized_head().await.map(|head| head.number));
        }
        server.abort();

        let stale = Err(ChainError::FinalizedHeadRegressed {
            previous: 47_446_696,
            current: 47_446_540,
        });
        assert_eq!(
            heads,
            vec![Ok(47_446_696), stale.clone(), stale, Ok(47_446_696)]
        );
    }

    // The incident's payment, read as the per-block scan and the confirm step read it: a type-2
    // transaction whose OP-stack receipt carries the L1 fee fields, with `blockTimestamp` on the log.
    #[tokio::test]
    async fn base_sepolia_transfer_decodes_from_recorded_answers() {
        let answers = vec![
            ("eth_getLogs", base_sepolia("transfer-logs")),
            ("eth_getTransactionReceipt", base_sepolia("receipt")),
            ("eth_getTransactionByHash", base_sepolia("transaction")),
            ("eth_getBlockByHash", base_sepolia("block-47445875")),
        ];
        let (scanner, scanner_node) = replay_node(answers.clone(), Vec::new()).await;
        let (confirm, confirm_node) = replay_node(answers, Vec::new()).await;
        let recipient = address!("0xfa810b787da3f2ca13fc13082762e78c4104ab10");
        let tx_hash = b256!("0x4b6cf1a33019535930118d535e51966a0405d78874213f5e34e5df2eb223902f");
        let logs = scanner
            .transfer_logs_to(&[recipient], 47_445_875, 47_445_875)
            .await;
        let lookup = confirm.receipt_transfer(tx_hash, 0).await;
        scanner_node.abort();
        confirm_node.abort();

        let block_hash =
            b256!("0xcac908304ca374430510276e42beb9f0f28596b6d925097e9b192913c6d195a2");
        let payer = address!("0x1d49cc344c26be92c0f941064412dd258f026b96");
        let transfer = TransferLog {
            tx_hash,
            receipt_log_index: 0,
            log_index: 35,
            block_number: 47_445_875,
            block_hash,
            block_time: DateTime::parse_from_rfc3339("2026-09-29T05:33:58Z")
                .expect("time")
                .into(),
            tx_from: payer,
            tx_nonce: 4,
            token: address!("0x1a6f260377e42ead1418c7c1afdfd5de371a9284"),
            from: payer,
            to: recipient,
            amount: AtomicAmount::new(U256::from(81_209_600_000_000_000_000_u128)),
        };
        assert_eq!(logs.expect("transfer logs"), vec![transfer.clone()]);
        assert_eq!(
            lookup.expect("receipt lookup"),
            ReceiptLookup::Included {
                block_number: 47_445_875,
                block_hash,
                transfer: Some(Box::new(transfer)),
            }
        );
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
        let derived = client
            .factory_addresses(Address::ZERO, Address::ZERO, &salts)
            .await;
        server.abort();

        assert_eq!(tokens.expect("token balances"), vec![U256::from(1); count]);
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

    async fn finalized_block_error(status: u16, error: &'static str) -> ChainError {
        let (client, server) = mock_node(status, error).await;
        let result = client.finalized_block().await;
        server.abort();
        let error = result.expect_err("node rejects the request");
        assert_redacted(&error, &client);
        error
    }

    #[tokio::test]
    async fn node_rejection_reason_reaches_the_operator_without_the_url() {
        let error =
            finalized_block_error(200, r#"{"code":-32000,"message":"header not found"}"#).await;
        let display = error.to_string();
        assert!(
            display.contains(
                "finalized block failed for provider `provider-a` \
                 (JSON-RPC error -32000: header not found)"
            ),
            "{display}"
        );
    }

    // alloy 2 surfaces a JSON-RPC error body on a non-2xx response as that JSON-RPC error, not as
    // an HTTP error, so the classification must rest on the payload alone.
    #[tokio::test]
    async fn http_errors_with_a_json_rpc_body_keep_their_rate_limit_class() {
        let error =
            finalized_block_error(502, r#"{"code":-32603,"message":"upstream failed"}"#).await;
        assert!(matches!(error, ChainError::Transport(_)), "{error:?}");
        assert!(!error.is_rate_limited(), "{error:?}");

        let error =
            finalized_block_error(429, r#"{"code":-32005,"message":"request rate exceeded"}"#)
                .await;
        assert!(matches!(error, ChainError::Transport(_)), "{error:?}");
        // Tenderly's public gateway refuses excess requests this way; callers may back off.
        assert!(error.is_rate_limited(), "{error:?}");
        assert!(
            error.to_string().contains(
                "finalized block failed for provider `provider-a` \
                 (JSON-RPC error -32005: request rate exceeded)"
            ),
            "{error}"
        );
    }
}
