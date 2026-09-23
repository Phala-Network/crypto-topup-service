//! Typed HTTP request and response models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// Account registration body.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RegisterAccountRequest {
    /// Product-owned account identifier.
    pub external_id: String,
}

/// Product-owned account.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AccountResponse {
    /// Service account identifier.
    pub id: Uuid,
    /// Product-owned account identifier.
    pub external_id: String,
    /// Workspace lifecycle state.
    pub status: String,
    /// Active account-level pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Inputs needed to recompute a persistent CREATE2 address.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PersistentSaltInputs {
    /// Stable product slug.
    pub product_slug: String,
    /// Product-owned account identifier.
    pub external_id: String,
    /// Persistent address version.
    pub version: u64,
}

/// Persistent deposit address and deterministic derivation inputs.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositAddressResponse {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Route name governing deposits to this address.
    pub route: String,
    /// Canonical EVM address.
    pub address: String,
    /// Canonical CREATE2 salt.
    pub salt: String,
    /// Inputs encoded into the salt.
    pub salt_inputs: PersistentSaltInputs,
}

/// Idempotent persistent-address rotation request.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RotateDepositAddressRequest {
    /// Address version the caller observed before requesting rotation.
    pub from_version: u64,
}

/// Deposit-list filters.
#[derive(Clone, Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DepositListQuery {
    /// Filter by state.
    pub state: Option<String>,
    /// Include deposits created at or after this time.
    pub from: Option<DateTime<Utc>>,
    /// Include deposits created before this time.
    pub to: Option<DateTime<Utc>>,
    /// Opaque cursor returned by the previous page.
    pub cursor: Option<Uuid>,
}

/// Product-wide support lookup filters.
#[derive(Clone, Debug, Default, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DepositLookupQuery {
    /// Canonical transaction hash.
    pub tx_hash: Option<String>,
    /// Canonical receiving address.
    pub address: Option<String>,
    /// Product lock reference.
    pub lock_ref: Option<String>,
    /// Opaque `(created_at, id)` cursor returned by the previous support page.
    pub cursor: Option<String>,
}

/// Product-visible deposit facts.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositResponse {
    /// Deterministic deposit identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Canonical transaction hash.
    pub tx_hash: String,
    /// Transfer log index.
    pub log_index: u64,
    /// Finalized block number.
    pub block_number: u64,
    /// Finalized block time.
    pub block_time: DateTime<Utc>,
    /// Receiving forwarder address.
    pub address: String,
    /// Rate-lock reference, when applicable.
    pub lock_ref: Option<String>,
    /// Selected route.
    pub route: Option<String>,
    /// Selected route version.
    pub route_version: Option<u64>,
    /// Canonical token contract address.
    pub asset_contract: String,
    /// Canonical transfer sender address.
    pub from_address: String,
    /// Atomic token amount encoded as a decimal string.
    pub amount_atomic: String,
    /// Current processing state.
    pub state: String,
    /// Valuation observation time.
    pub valuation_at: Option<DateTime<Utc>>,
    /// Eight-decimal scaled price encoded as a decimal string.
    pub price_scaled: Option<String>,
    /// Product credit in minor units encoded as a decimal string.
    pub credit_minor: Option<String>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last processing update time.
    pub updated_at: DateTime<Utc>,
}

/// One immutable state transition in a support timeline.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositTransitionResponse {
    /// Timeline row identifier.
    pub id: Uuid,
    /// State before the transition attempt.
    pub from_state: String,
    /// State after the transition attempt.
    pub to_state: String,
    /// Retry attempt recorded for this transition.
    pub attempt: i32,
    /// Durable transition evidence.
    pub evidence: serde_json::Value,
    /// Transition creation time.
    pub created_at: DateTime<Utc>,
}

/// Deposit facts with the full immutable transition timeline.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SupportDepositResponse {
    /// Product-visible deposit facts.
    #[serde(flatten)]
    pub deposit: DepositResponse,
    /// Transitions in ascending creation order.
    pub timeline: Vec<DepositTransitionResponse>,
}

/// A support lookup page with timelines.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct SupportDepositsResponse {
    /// Matching deposits in descending creation order.
    pub deposits: Vec<SupportDepositResponse>,
    /// Cursor for the next page, or `null` when exhausted.
    pub next_cursor: Option<String>,
}

/// A page of deposits.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositsResponse {
    /// Deposits in descending creation order.
    pub deposits: Vec<DepositResponse>,
    /// Cursor for the next page, or `null` when exhausted.
    pub next_cursor: Option<Uuid>,
}

/// Configured route limits and currently available exposure.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct LimitsResponse {
    /// Route name.
    pub route: String,
    /// Minimum atomic deposit amount.
    pub min_deposit_atomic: String,
    /// Maximum atomic deposit amount.
    pub max_deposit_atomic: String,
    /// Minimum destination credit in minor units.
    pub min_credit_minor: u64,
    /// Per-account open rate-lock cap in minor units.
    pub account_open_minor: u64,
    /// Per-product open rate-lock cap in minor units.
    pub product_open_minor: u64,
    /// Global open rate-lock cap in minor units.
    pub global_open_minor: u64,
    /// Remaining account exposure.
    pub remaining_account_minor: Option<u64>,
    /// Earliest open-lock expiry, when any exposure is reserved.
    pub reset_at: Option<DateTime<Utc>>,
}

/// Pause or resume request.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct PauseRequest {
    /// Pause scopes to add or remove.
    pub scopes: Vec<String>,
}

/// Current scopes after a pause mutation.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PauseResponse {
    /// Current pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Rate-lock creation body owned by C10.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct CreateRateLockRequest {
    /// Desired destination amount in minor units.
    pub amount_minor: Option<String>,
    /// Desired token amount in atomic units.
    pub amount_atomic: Option<String>,
    /// Product checkout reference.
    pub product_lock_ref: String,
}

/// Rate-lock response shape owned by C10.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RateLockResponse {
    /// Single-use forwarder address.
    pub address: String,
    /// Exact token amount in atomic units.
    pub amount_atomic: String,
    /// Locked eight-decimal scaled price.
    pub price_scaled: String,
    /// Destination credit in minor units.
    pub credit_minor: String,
    /// Lock expiry time.
    pub expires_at: DateTime<Utc>,
    /// Stable lifecycle status.
    pub status: String,
    /// Whole seconds remaining while the lock is open.
    pub remaining_seconds: u64,
    /// EIP-681 payment URI.
    pub eip681_uri: String,
    /// Inputs encoded into the rate-lock salt.
    pub salt_inputs: RateLockSaltInputs,
    /// The payment to the lock address that the checkout page should show, once one is seen on
    /// chain: the deposit that consumed the lock; otherwise the first payment that would consume
    /// it; otherwise the first payment. Display only: while `status` is `seen` the payment is not
    /// final and nothing has been credited.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub payment: Option<RateLockPayment>,
}

/// A payment observed at a rate-lock address.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RateLockPayment {
    /// `seen`: in a block above `finalized`, provisional and may still disappear in a reorg.
    /// `finalized`: recorded as a deposit; follow it by `deposit_id`. New values may be added.
    pub status: String,
    /// Identifier the deposit has, or will have once final.
    pub deposit_id: Uuid,
    /// Canonical transaction hash.
    pub tx_hash: String,
    /// Transfer log index.
    pub log_index: u64,
    /// Block that contains the transfer.
    pub block_number: u64,
    /// Blocks on top of and including that block at the last head scan; `seen` only.
    pub confirmations: Option<u64>,
    /// Atomic token amount encoded as a decimal string.
    pub amount_atomic: String,
    /// Canonical token contract address.
    pub asset_contract: String,
    /// Whether the token is the lock's route asset.
    pub supported: bool,
    /// Whether the amount is the lock's asset within the lock tolerance; always false on a
    /// cancelled lock.
    pub amount_within_tolerance: bool,
    /// Whether the block time is at or before `expires_at`; always false on a cancelled lock.
    pub in_time: bool,
    /// Estimated finality time: block time plus 15 minutes; `seen` only.
    pub estimated_final_at: Option<DateTime<Utc>>,
}

/// A transfer to a persistent address seen above the finalized head. It is not a deposit, has not
/// been credited, and may disappear in a reorg; once final it appears under `deposits`.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PendingDepositResponse {
    /// Identifier the deposit will have once final.
    pub deposit_id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Canonical transaction hash.
    pub tx_hash: String,
    /// Transfer log index.
    pub log_index: u64,
    /// Block that contains the transfer.
    pub block_number: u64,
    /// Block time.
    pub block_time: DateTime<Utc>,
    /// Blocks on top of and including that block at the last head scan.
    pub confirmations: u64,
    /// Receiving forwarder address.
    pub address: String,
    /// Canonical token contract address.
    pub asset_contract: String,
    /// Canonical transfer sender address.
    pub from_address: String,
    /// Atomic token amount encoded as a decimal string.
    pub amount_atomic: String,
    /// Whether a route of this product accepts this token on this chain. Only routed tokens are
    /// scanned before finality, so this is false only for a token routed for another product.
    pub supported: bool,
    /// First time the service saw the transfer.
    pub first_seen_at: DateTime<Utc>,
    /// Estimated finality time: block time plus 15 minutes.
    pub estimated_final_at: DateTime<Utc>,
}

/// Pending transfers to an account's persistent addresses.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct PendingDepositsResponse {
    /// Pending transfers in block order.
    pub pending_deposits: Vec<PendingDepositResponse>,
}

/// Inputs needed to recompute a rate-lock CREATE2 address.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RateLockSaltInputs {
    /// Stable product slug.
    pub product_slug: String,
    /// Product-owned account identifier.
    pub external_id: String,
    /// Product checkout reference.
    pub lock_ref: String,
}

/// Cancellation result for an unpaid rate lock.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct CancelRateLockResponse {
    /// Product checkout reference.
    pub product_lock_ref: String,
    /// Stable cancellation status.
    pub status: String,
}

/// Refund request body owned by C12.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RefundRequest {
    /// Customer-controlled destination address.
    pub to_address: String,
    /// Atomic refund amount.
    pub amount: String,
}

/// Administrative refund record body owned by C12.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RecordRefundRequest {
    /// Treasury transaction hash to verify at finalized.
    pub tx_hash: String,
}

/// Customer refund request accepted for finance review.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RefundResponse {
    /// Refund request identifier.
    pub id: Uuid,
    /// Related deposit identifier.
    pub deposit_id: Uuid,
    /// Atomic token amount.
    pub amount_atomic: String,
    /// Customer-controlled destination address.
    pub to_address: String,
    /// Stable workflow status.
    pub status: String,
}

/// Administrative deposit nudge result.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct NudgeResponse {
    /// Nudged deposit identifier.
    pub deposit_id: Uuid,
    /// Newly due processing time.
    pub next_attempt_at: DateTime<Utc>,
}

/// Administrative refund workflow result.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AdminRefundResponse {
    /// Refund request identifier.
    pub id: Uuid,
    /// Stable workflow status.
    pub status: String,
    /// Recorded treasury transaction hash, when present.
    pub tx_hash: Option<String>,
    /// Most recent confirmation evidence, when checked.
    pub confirmation_evidence: Option<serde_json::Value>,
}

/// Per-route daily finance report produced by C12.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RouteDailyReport {
    /// Stable route name.
    pub route: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Route asset contract.
    pub asset_contract: String,
    /// Latest treasury token balance in atomic units, when the chain read succeeds.
    pub treasury_balance_atomic: Option<String>,
    /// Balance source or explicit reason the treasury balance is unavailable.
    pub treasury_balance_note: String,
    /// Sum of deposits not linked to a confirmed flush.
    pub unflushed_balance_atomic: String,
    /// Sum of unconsumed rate-lock token amounts.
    pub open_rate_lock_exposure_atomic: String,
    /// Open lock exposure in destination minor units, when available.
    pub exposure_minor: Option<String>,
    /// Reason destination exposure is unavailable.
    pub exposure_minor_reason: String,
    /// Route PnL in destination minor units, when available.
    pub pnl_minor: Option<String>,
    /// Reason route PnL is unavailable.
    pub pnl_minor_reason: String,
    /// Rejected token amount still held after confirmed refunds.
    pub rejected_holds_atomic: String,
    /// Deposit counts keyed by state.
    pub deposits_by_state: std::collections::BTreeMap<String, u64>,
    /// Settlement counts keyed by status.
    pub settlements_by_status: std::collections::BTreeMap<String, u64>,
    /// Refund counts keyed by status.
    pub refunds_by_status: std::collections::BTreeMap<String, u64>,
    /// Maximum age in seconds keyed by current deposit state.
    pub age_in_state_max_seconds: std::collections::BTreeMap<String, u64>,
}

/// Daily finance report produced by C12.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DailyReportResponse {
    /// Report snapshot time.
    pub generated_at: DateTime<Utc>,
    /// SQL-computed metrics for each configured route.
    pub routes: Vec<RouteDailyReport>,
}

/// Administrative route pause response.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct RoutePauseResponse {
    /// Route name.
    pub route: String,
    /// Current route-level pause scopes.
    pub paused_scopes: Vec<String>,
}

/// Attestation query parameters.
#[derive(Clone, Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct AttestationQuery {
    /// Non-empty hexadecimal nonce of at most 32 bytes.
    pub nonce: String,
}

/// TDX evidence binding a nonce to the settlement public key.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AttestationResponse {
    /// Settlement key identifier.
    pub keyid: String,
    /// Raw ed25519 settlement public key as lowercase hexadecimal.
    pub settlement_pubkey: String,
    /// SHA-256 report data as lowercase hexadecimal.
    pub report_data: String,
    /// Versioned dstack attestation bytes as lowercase hexadecimal.
    pub quote: String,
}
