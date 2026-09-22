use std::error::Error;
use std::fmt::{self, Display, Formatter};

use alloy_primitives::{Address, B256, Bytes, U256};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use topup_core::money::ScaledPrice;
use uuid::Uuid;

/// One fee suggestion for an EIP-1559 transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FeeQuote {
    /// Maximum total fee per gas unit.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas unit.
    pub max_priority_fee_per_gas: u128,
}

/// One EVM receipt log with the position fields required by custody accounting.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainLog {
    /// Contract which emitted the log.
    pub address: Address,
    /// Indexed event topics.
    pub topics: Vec<B256>,
    /// ABI-encoded non-indexed event data.
    pub data: Bytes,
    /// Block containing the log.
    pub block_number: u64,
    /// Global log index within the block.
    pub log_index: u64,
}

/// Receipt information used by the flusher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainReceipt {
    /// Transaction hash selected by the chain.
    pub transaction_hash: B256,
    /// Block containing the transaction.
    pub block_number: u64,
    /// Whether EVM execution succeeded.
    pub success: bool,
    /// Receipt logs.
    pub logs: Vec<ChainLog>,
}

/// One bounded sender/nonce recovery scan result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NonceReceiptSearch {
    /// Matching receipt when found in the scanned window.
    pub receipt: Option<ChainReceipt>,
    /// First block of the next window, or `None` when the latest block was reached.
    pub next_block: Option<u64>,
}

/// Chain operations required by planning, sending, confirmation, and recovery.
#[async_trait]
pub trait ChainClient: Send + Sync {
    /// Reads ERC-20 balances in one JSON-RPC batch.
    async fn token_balances(
        &self,
        token: Address,
        addresses: &[Address],
    ) -> Result<Vec<U256>, ChainError>;

    /// Reads native balances in one JSON-RPC batch.
    async fn native_balances(&self, addresses: &[Address]) -> Result<Vec<U256>, ChainError>;

    /// Estimates one factory flush transaction.
    async fn estimate_flush_gas(
        &self,
        factory: Address,
        operator: Address,
        salts: &[B256],
        token: Address,
    ) -> Result<u64, ChainError>;

    /// Reports whether `operator` holds `OPERATOR_ROLE` on the factory at the latest block.
    async fn has_operator_role(
        &self,
        factory: Address,
        operator: Address,
    ) -> Result<bool, ChainError>;

    /// Returns the pending nonce for an operator.
    async fn pending_nonce(&self, operator: Address) -> Result<u64, ChainError>;

    /// Returns the latest mined nonce for recovery consumption checks.
    async fn confirmed_nonce(&self, operator: Address) -> Result<u64, ChainError>;

    /// Returns the latest observed block number.
    async fn latest_block(&self) -> Result<u64, ChainError>;

    /// Returns the reviewed finalized block number.
    async fn finalized_block(&self) -> Result<u64, ChainError>;

    /// Returns an EIP-1559 fee suggestion.
    async fn fee_quote(&self) -> Result<FeeQuote, ChainError>;

    /// Broadcasts a previously signed EIP-2718 transaction.
    async fn send_raw_transaction(&self, raw: &[u8]) -> Result<B256, ChainError>;

    /// Reads a transaction receipt by hash.
    async fn receipt(&self, hash: B256) -> Result<Option<ChainReceipt>, ChainError>;

    /// Locates a consumed operator nonce when the known hash was replaced externally.
    async fn receipt_by_sender_nonce(
        &self,
        operator: Address,
        nonce: u64,
        from_block: u64,
        max_blocks: u64,
    ) -> Result<NonceReceiptSearch, ChainError>;
}

/// Primary route-price source used by the planner.
#[async_trait]
pub trait PriceSource: Send + Sync {
    /// Returns the latest validated USD price for one provider asset identifier.
    async fn price_usd(&self, asset: &str) -> Result<ScaledPrice, PriceError>;
}

/// Operational notification emitted by the flusher.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FlushAlert {
    /// A forwarder holds native ETH; C7 does not automatically flush it.
    NativeBalance {
        /// Chain containing the balance.
        chain_id: u64,
        /// Forwarder address.
        address: Address,
        /// Native amount in wei.
        amount: U256,
    },
    /// A flush transaction reverted.
    Reverted {
        /// Flush row identifier.
        flush_id: Uuid,
        /// Reverted operator nonce.
        nonce: u64,
    },
    /// Repeated bisection isolated a persistently failing forwarder.
    IsolatedAddress {
        /// Chain containing the forwarder.
        chain_id: u64,
        /// Token whose flush failed.
        token: Address,
        /// Address row identifier.
        address_id: Uuid,
        /// Physical forwarder address.
        address: Address,
        /// CREATE2 salt.
        salt: B256,
    },
    /// A nonce is consumed but no receipt could be located.
    MissingConsumedReceipt {
        /// Chain containing the transaction.
        chain_id: u64,
        /// Operator account.
        operator: Address,
        /// Consumed nonce.
        nonce: u64,
    },
    /// Planning excluded an address whose singleton estimate reverted.
    PlanningExcluded {
        /// Chain containing the forwarder.
        chain_id: u64,
        /// Token whose flush estimate reverted.
        token: Address,
        /// Address row identifier.
        address_id: Uuid,
        /// Human-readable exclusion reason.
        reason: String,
    },
    /// A replacement could not satisfy the required bump below the configured cap.
    FeeCapReached {
        /// Flush row identifier.
        flush_id: Uuid,
        /// Required maximum fee per gas.
        required_max_fee_per_gas: u128,
        /// Configured maximum fee per gas.
        cap: u128,
    },
}

/// Sink for flusher alerts and alert metrics.
pub trait AlertSink: Send + Sync {
    /// Records one alert occurrence.
    fn emit(&self, alert: FlushAlert);
}

/// No-op alert sink for applications which wire metrics separately.
pub struct NoopAlertSink;

impl AlertSink for NoopAlertSink {
    fn emit(&self, _alert: FlushAlert) {}
}

/// Runtime fee and replacement policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlusherPolicy {
    /// Blocks to wait before replacing an unmined transaction.
    pub replacement_after_blocks: u64,
    /// Replacement multiplier in basis points.
    pub replacement_bps: u16,
    /// Hard cap for the total fee per gas unit.
    pub max_fee_per_gas: u128,
    /// Gas-limit multiplier in basis points over `eth_estimateGas`.
    pub gas_limit_bps: u16,
    /// Maximum blocks inspected per consumed-nonce recovery iteration.
    pub recovery_scan_blocks: u64,
}

impl Default for FlusherPolicy {
    fn default() -> Self {
        Self {
            replacement_after_blocks: 3,
            replacement_bps: 12_500,
            max_fee_per_gas: 500_000_000_000,
            gas_limit_bps: 12_000,
            recovery_scan_blocks: 128,
        }
    }
}

/// A forwarder selected into a durable flush plan.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlannedAddress {
    /// Database address row.
    pub address_id: Uuid,
    /// CREATE2 salt as canonical hex.
    pub salt: String,
    /// Physical address as canonical hex.
    pub address: String,
    /// Observed token balance as decimal text.
    pub balance_atomic: String,
}

/// One signed transaction version retained for recovery and fee replacement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedVersion {
    /// Transaction hash as canonical hex.
    pub hash: String,
    /// EIP-2718 bytes as lowercase hex without a secret key.
    pub raw: String,
    /// Block number when this version was persisted.
    pub signed_at_block: u64,
    /// Maximum total fee per gas unit.
    pub max_fee_per_gas: u128,
    /// Maximum priority fee per gas unit.
    pub max_priority_fee_per_gas: u128,
}

/// JSON shape stored in `flushes.receipt` throughout the transaction lifecycle.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlushEvidence {
    /// Immutable route and contract binding used by every lifecycle phase.
    pub binding: FlushCallBinding,
    /// Planned addresses and salts.
    pub plan: Vec<PlannedAddress>,
    /// Gas estimate used for signing.
    pub estimated_gas: u64,
    /// Number of prior failures on the same salt group.
    pub failure_attempt: u32,
    /// Parent flush when this plan was created by revert handling.
    pub parent_flush_id: Option<Uuid>,
    /// Signed transaction versions, newest last.
    pub signed: Vec<SignedVersion>,
    /// Final chain receipt serialized for operations and reconciliation.
    pub chain_receipt: Option<serde_json::Value>,
    /// Next block to inspect when recovering a consumed nonce.
    pub recovery_from_block: Option<u64>,
}

impl FlushEvidence {
    /// Creates evidence for a newly planned batch.
    #[must_use]
    pub const fn planned(
        binding: FlushCallBinding,
        plan: Vec<PlannedAddress>,
        estimated_gas: u64,
    ) -> Self {
        Self {
            binding,
            plan,
            estimated_gas,
            failure_attempt: 0,
            parent_flush_id: None,
            signed: Vec::new(),
            chain_receipt: None,
            recovery_from_block: None,
        }
    }
}

/// Immutable factory call binding persisted with a planned flush.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlushCallBinding {
    /// Stable route name used when the plan was created.
    pub route: String,
    /// Attested route configuration version.
    pub config_version: u64,
    /// Factory address as canonical hex.
    pub factory: String,
    /// Token address as canonical hex.
    pub token: String,
    /// Complete ordered salt parameters as canonical hex.
    pub salts: Vec<String>,
}

/// Error returned by an EVM chain adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainError {
    message: String,
    estimation_revert: bool,
}

impl ChainError {
    /// Creates an ordinary RPC or decoding failure.
    #[must_use]
    pub fn rpc(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            estimation_revert: false,
        }
    }

    /// Creates an execution revert returned by `eth_estimateGas`.
    #[must_use]
    pub fn estimation_revert(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            estimation_revert: true,
        }
    }

    /// Returns whether this error is a deterministic estimate execution revert.
    #[must_use]
    pub const fn is_estimation_revert(&self) -> bool {
        self.estimation_revert
    }
}

impl Display for ChainError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for ChainError {}

/// Error returned by a price adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PriceError(pub String);

impl Display for PriceError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for PriceError {}
