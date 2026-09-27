//! Typed HTTP request and response models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// A deposit's stored facts, for the operator (`GET /v1/admin/deposits/{id}`).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositResponse {
    /// Deterministic deposit identifier.
    pub id: Uuid,
    /// Product-owned identifier of the account the receiving address belongs to. This service
    /// always sends it; it is optional in the schema so clients also parse responses from servers
    /// that predate it.
    #[schema(required = false)]
    pub external_id: String,
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
    /// Which price valued the deposit: `lock` (the quoted price) or `spot`.
    pub price_source: Option<String>,
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
    /// Webhook events about the deposit in ascending creation order. This service always sends
    /// it; it is optional in the schema so clients also parse responses from servers that predate
    /// it.
    #[schema(required = false)]
    pub events: Vec<DepositEventResponse>,
}

/// One webhook event about a deposit and its delivery state.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositEventResponse {
    /// Stable event identifier, sent as the `webhook-id` header: `evt_…`, or the UUID of an
    /// older event.
    pub id: String,
    /// Event type, such as `deposit.credited`.
    pub event_type: String,
    /// Event creation time.
    pub created_at: DateTime<Utc>,
    /// When the receiver accepted the event, or `null` while it is undelivered.
    pub delivered_at: Option<DateTime<Utc>>,
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

/// `POST /v1/quotes` body.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateQuoteRequest {
    /// Your identifier of the customer account to credit, 1 to 255 bytes; the account is created
    /// on its first quote.
    pub account_id: String,
    /// The credit to quote, a positive integer in the currency's minor unit (US cents).
    pub amount: u64,
    /// Lowercase ISO currency code; only `usd`.
    pub currency: String,
    /// EVM chain of the payment, one of `GET /v1/config` `assets[].chain_id`.
    pub chain_id: u64,
    /// Asset code of the payment on that chain, such as `pha`.
    pub asset: String,
}

/// A quote: a locked price, an exact token amount, and a single-use address to pay it to.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Quote {
    /// `qt_` id. New quotes' address salt is `keccak256(abi.encode(product_slug, account_id,
    /// "lock", id))`.
    pub id: String,
    /// Always `quote`.
    pub object: String,
    /// Your account identifier.
    pub account_id: String,
    /// Credit in the currency's minor unit.
    pub amount: u64,
    /// `usd`.
    pub currency: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Asset code.
    pub asset: String,
    /// The exact token amount to pay, in base units, as a decimal string.
    pub amount_atomic: String,
    /// The locked price in USD per token, a decimal string with 8 decimal places.
    pub exchange_rate: String,
    /// Single-use forwarder address to pay.
    pub address: String,
    /// EIP-681 URI carrying the token, chain, address, and amount.
    pub payment_uri: String,
    /// `open`, `complete` (a matching payment consumed it), `expired`, or `canceled`. A quote stays
    /// `open` after `expires_at` until the finalized chain passes it, so a payment mined in time
    /// is never reported as expired; hide the address once `expires_at` has passed.
    pub status: String,
    /// End of the payment window, Unix seconds.
    pub expires_at: i64,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// The payment the checkout page should show, once one is seen on chain; display only.
    pub payment: Option<QuotePayment>,
    /// The deposit that completed the quote: its `dep_` id, or the object with `expand[]=deposit`.
    pub deposit: Option<ExpandableDeposit>,
    /// Lets the payer's browser read the quote's public view, `ClientQuote`, from
    /// `GET /v1/quotes/{id}?client_secret=…` without your signature. Returned only by
    /// `POST /v1/quotes`, since only its hash is stored; a repeat with the same `Idempotency-Key`
    /// returns a new secret and the earlier one stops working. Give it only to the paying
    /// customer's page, and do not log it.
    pub client_secret: Option<String>,
}

/// The public view of a quote, read with its `client_secret` and without a signature, for the
/// payer's checkout page. It has no account or internal fields.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ClientQuote {
    /// `qt_` id.
    pub id: String,
    /// Always `quote`.
    pub object: String,
    /// `open`, `complete`, `expired`, or `canceled`, as on `Quote`; hide the address once
    /// `expires_at` has passed.
    pub status: String,
    /// Credit in the currency's minor unit.
    pub amount: u64,
    /// `usd`.
    pub currency: String,
    /// Asset code.
    pub asset: String,
    /// The token's decimals, to display `amount_atomic`.
    pub decimals: u8,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The exact token amount to pay, in base units, as a decimal string.
    pub amount_atomic: String,
    /// Single-use forwarder address to pay.
    pub address: String,
    /// EIP-681 URI carrying the token, chain, address, and amount.
    pub payment_uri: String,
    /// End of the payment window, Unix seconds.
    pub expires_at: i64,
    /// Progress of the payment shown on the page; display only, never a reason to deliver
    /// anything: `none`; `seen` (in a block that is not final yet and may still disappear);
    /// `confirming` (final, being valued and screened); `credited`; or `rejected` (final and not
    /// credited; the payer should contact the product's support).
    pub payment_status: String,
    /// While `seen`: blocks on top of and including the payment's block; otherwise `null`.
    pub confirmations: Option<u64>,
}

/// `GET /v1/quotes/{id}` returns a `Quote` to a signed request and a `ClientQuote` to a request by
/// `client_secret`.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(untagged)]
pub enum QuoteView {
    /// The product's view.
    Quote(Quote),
    /// The payer's view.
    Client(ClientQuote),
}

/// A payment observed at a quote's address. Display only: while `status` is `seen` it is not
/// final, may still disappear in a reorg, and nothing has been credited.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct QuotePayment {
    /// `seen` (above the finalized head) or `final` (recorded as a deposit). New values may be
    /// added.
    pub status: String,
    /// Canonical transaction hash.
    pub tx_hash: String,
    /// Token amount in base units, as a decimal string.
    pub amount_atomic: String,
    /// Blocks on top of and including the transfer's block at the last head scan; `seen` only.
    pub confirmations: Option<u64>,
    /// Estimated finality time, Unix seconds: block time plus 15 minutes; `seen` only.
    pub estimated_final_at: Option<i64>,
    /// Whether the payment is the quote's asset, in time, and within tolerance, so it will be
    /// credited at the quoted price; otherwise it is credited at spot once final.
    pub matches_quote: bool,
    /// `dep_` id the deposit has, or will have once final.
    pub deposit: String,
}

/// A quote id, or the quote with `expand[]`.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(untagged)]
#[schema(no_recursion)]
pub enum ExpandableQuote {
    /// `qt_` id.
    Id(String),
    /// The expanded quote.
    Object(Box<Quote>),
}

/// A deposit id, or the deposit with `expand[]`.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(untagged)]
#[schema(no_recursion)]
pub enum ExpandableDeposit {
    /// `dep_` id.
    Id(String),
    /// The expanded deposit.
    Object(Box<Deposit>),
}

/// A final transfer to a quote's address: valued, screened, and credited, or rejected.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Deposit {
    /// `dep_` and the hex of the deposit's deterministic UUID,
    /// `uuid_v5(DEPOSIT_NAMESPACE, "{chain_id}:{tx_hash}:{log_index}")`.
    pub id: String,
    /// Always `deposit`.
    pub object: String,
    /// Your account identifier.
    pub account_id: String,
    /// The quote whose address received the transfer; `null` for a persistent address.
    pub quote: Option<ExpandableQuote>,
    /// `detected`, `confirmed`, `credited`, `swept`, or `rejected`. New values may be added.
    pub status: String,
    /// Why the deposit was rejected: `unsupported_asset`, `below_minimum`, `out_of_bounds`,
    /// `out_of_range`, `sanctioned`, or `product_refused` (historical).
    pub rejection_reason: Option<String>,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Asset code; `null` for a token without a route.
    pub asset: Option<String>,
    /// Token contract address.
    pub asset_contract: String,
    /// Token amount in base units, as a decimal string.
    pub amount_atomic: String,
    /// Credit in the currency's minor unit (cents), once valued.
    pub amount: Option<u64>,
    /// `usd`.
    pub currency: String,
    /// USD per token, a decimal string with 8 places, once valued.
    pub exchange_rate: Option<String>,
    /// `quote` (the quoted price) or `spot`, once valued.
    pub price_source: Option<String>,
    /// Valuation time, Unix seconds.
    pub valued_at: Option<i64>,
    /// Receiving forwarder address.
    pub address: String,
    /// Sender of the transfer.
    pub from_address: String,
    /// Transaction hash.
    pub tx_hash: String,
    /// Log index of the transfer.
    pub log_index: u64,
    /// Finalized block number.
    pub block_number: u64,
    /// Refunded token amount in base units, as a decimal string: the sum of succeeded refunds.
    pub amount_refunded_atomic: String,
    /// Whether the deposit is fully refunded.
    pub refunded: bool,
    /// Detection time, Unix seconds.
    pub created: i64,
}

/// A page of a list, newest first (<https://docs.stripe.com/api/pagination>).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositList {
    /// Always `list`.
    pub object: String,
    /// The list's path, `/v1/deposits`.
    pub url: String,
    /// Whether more deposits follow in the direction of this page.
    pub has_more: bool,
    /// The deposits.
    pub data: Vec<Deposit>,
}

/// `POST /v1/refunds` body.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateRefundRequest {
    /// `dep_` id of the deposit to refund.
    pub deposit: String,
    /// Address the customer controls; never default it to the sender, which may be an exchange.
    pub destination_address: String,
    /// Amount in base units, as a decimal string; the unrefunded remainder when absent.
    pub amount_atomic: Option<String>,
}

/// A refund of (part of) a deposit to the customer, executed by finance from the treasury.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Refund {
    /// `re_` id.
    pub id: String,
    /// Always `refund`.
    pub object: String,
    /// The refunded deposit: its `dep_` id, or the object with `expand[]=deposit`.
    pub deposit: ExpandableDeposit,
    /// Token amount in base units, as a decimal string.
    pub amount_atomic: String,
    /// Destination address.
    pub destination_address: String,
    /// `pending` (requested, approved, or sent) or `succeeded` (the transfer is final).
    pub status: String,
    /// Refund transaction hash, once sent.
    pub tx_hash: Option<String>,
    /// Request time, Unix seconds.
    pub created: i64,
}

/// What a product's UI reads instead of hardcoding: assets, limits, and quote terms.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Config {
    /// Always `config`.
    pub object: String,
    /// Credit currency, `usd`.
    pub currency: String,
    /// Per-account cap on the credit of open quotes, in cents; no single quote can exceed it.
    pub max_open_amount_per_account: u64,
    /// One entry per payable asset.
    pub assets: Vec<ConfigAsset>,
}

/// A payable asset and its terms.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ConfigAsset {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Asset code.
    pub asset: String,
    /// Token contract address.
    pub contract: String,
    /// Token decimals.
    pub decimals: u8,
    /// `spot` or `stablecoin`.
    pub pricing: String,
    /// Minimum credit in cents, for quotes and deposits; smaller deposits are not credited.
    pub min_amount: u64,
    /// Maximum creditable deposit in base units, as a decimal string.
    pub max_deposit_atomic: String,
    /// Minimum refundable amount in base units, as a decimal string.
    pub min_refund_atomic: String,
    /// Payment window of a quote, in seconds.
    pub quote_ttl_seconds: u64,
    /// A quote's price is spot / (1 + spread_bps / 10 000); spot-valued payments carry no spread.
    pub quote_spread_bps: u16,
    /// A payment within this many basis points of the quoted amount completes the quote.
    pub quote_tolerance_bps: u16,
    /// Typical time from payment to finality, in seconds.
    pub typical_finality_seconds: u64,
}

/// Administrative refund record body owned by C12.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RecordRefundRequest {
    /// Treasury transaction hash to verify at finalized.
    pub tx_hash: String,
}

/// Administrative product registration body. The product's key id is not part of it: the
/// attested route is its only source.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct RegisterProductRequest {
    /// Product slug named by a loaded route's `destination.product`; matches
    /// `^[a-z0-9][a-z0-9-]{0,62}$`.
    pub slug: String,
    /// Standard base64 of the product's 32-byte ed25519 request-verification public key.
    pub public_key: String,
    /// Absolute `https` URL of the product's webhook receiver; `http` only when the service's
    /// own public origin uses `http` (local stacks).
    pub webhook_url: String,
}

/// Administrative replacement of an issued product's verification key and webhook URL. The key id
/// stays the route's `destination.product_kid`.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct UpdateProductRequest {
    /// Standard base64 of the product's new 32-byte ed25519 request-verification public key.
    pub public_key: String,
    /// Absolute `https` URL of the product's webhook receiver; `http` only when the service's
    /// own public origin uses `http` (local stacks).
    pub webhook_url: String,
    /// Why the credentials change, 1 to 1024 bytes: the rotation or incident it rests on.
    pub reason: String,
}

/// Registered product.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ProductResponse {
    /// Service product identifier.
    pub id: Uuid,
    /// Product slug.
    pub slug: String,
    /// Standard base64 of the product's ed25519 public key.
    pub public_key: String,
    /// Webhook receiver URL.
    pub webhook_url: String,
    /// Active product-level pause scopes.
    pub paused_scopes: Vec<String>,
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

/// Administrative action body; `reason` is recorded in the action's audit row.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct AdminReasonRequest {
    /// Why the action is taken, 1 to 1024 bytes: the incident or sign-off it rests on.
    pub reason: String,
}

/// A lifted reconciliation block.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ReconciliationBlockLiftResponse {
    /// Lifted block, `chain:{chain_id}` or `address:{address_id}`.
    pub block_key: String,
    /// When the block was lifted; a repeated lift returns the original time.
    pub lifted_at: DateTime<Utc>,
}

/// A webhook event queued for delivery again.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OutboxReplayResponse {
    /// Stable event identifier, sent as the `webhook-id` header: `evt_…`, or the UUID of an
    /// older event.
    pub event_id: String,
    /// Event type, such as `deposit.credited`.
    pub event_type: String,
    /// When the delivery worker next attempts the event.
    pub next_attempt_at: DateTime<Utc>,
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
    /// Rejected token amount still held after confirmed refunds.
    pub rejected_holds_atomic: String,
    /// Deposit counts keyed by state.
    pub deposits_by_state: std::collections::BTreeMap<String, u64>,
    /// Credited deposits whose `deposit.credited` webhook the product has not acknowledged yet.
    pub credited_undelivered: u64,
    /// Age in seconds of the oldest of those events; zero when every one was delivered.
    pub credited_undelivered_max_age_seconds: u64,
    /// Refund counts keyed by status.
    pub refunds_by_status: std::collections::BTreeMap<String, u64>,
    /// Maximum age in seconds keyed by current deposit state.
    pub age_in_state_max_seconds: std::collections::BTreeMap<String, u64>,
    /// Latest scheduled flush planning run of this route in the serving process; absent for
    /// unrouted assets and until the first run after a restart.
    pub flush_planning: Option<FlushPlanningReport>,
}

/// Outcome of one scheduled flush planning run.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct FlushPlanningReport {
    /// When the run finished.
    pub at: DateTime<Utc>,
    /// `planned`; `idle` when no address met the flush policy, a flush was already in flight, or
    /// reconciliation froze the chain; `operator_not_authorized` when the operator was not known
    /// to hold `OPERATOR_ROLE`; `failed`; or `send_failed` when planning succeeded but sending or
    /// maintaining a flush afterwards failed.
    pub outcome: String,
    /// Error of a failed run, without provider URLs.
    pub error: Option<String>,
}

impl From<crate::observability::FlushPlanningStatus> for FlushPlanningReport {
    fn from(status: crate::observability::FlushPlanningStatus) -> Self {
        Self {
            at: status.at,
            outcome: status.outcome.code().to_owned(),
            error: status.error,
        }
    }
}

/// Latest reconciliation round of the serving process.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ReconciliationRoundReport {
    /// When the round finished.
    pub at: DateTime<Utc>,
    /// Checks that could not complete; empty after a complete round.
    pub failed_checks: Vec<FailedCheckReport>,
}

/// A persistent reconciliation block (architecture §13).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ReconciliationBlockReport {
    /// Block identifier, `chain:{chain_id}` or `address:{address_id}`.
    pub block_key: String,
    /// `chain` (the chain is frozen) or `address` (the address is left out of flush planning).
    pub scope: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Blocked address for an `address` block.
    pub address_id: Option<Uuid>,
    /// Check that wrote the block, such as `address_derivation`.
    pub check: String,
    /// Why the check blocked.
    pub reason: String,
    /// When the block was written.
    pub created_at: DateTime<Utc>,
}

/// One reconciliation check that could not complete.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct FailedCheckReport {
    /// Check code, such as `custody_balance`.
    pub check: String,
    /// Error that stopped the check, without provider URLs.
    pub error: String,
}

impl From<crate::observability::ReconciliationStatus> for ReconciliationRoundReport {
    fn from(status: crate::observability::ReconciliationStatus) -> Self {
        Self {
            at: status.at,
            failed_checks: status
                .failed_checks
                .into_iter()
                .map(|(check, error)| FailedCheckReport { check, error })
                .collect(),
        }
    }
}

/// Daily finance report produced by C12.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DailyReportResponse {
    /// Report snapshot time.
    pub generated_at: DateTime<Utc>,
    /// Open rate-lock credit across all products in destination minor units: the sum the global
    /// exposure cap is enforced against. This service always sends it; it is optional in the
    /// schema so clients also parse reports from servers that predate it.
    pub exposure_minor: Option<String>,
    /// SQL-computed metrics for each configured route.
    pub routes: Vec<RouteDailyReport>,
    /// Latest reconciliation round of the serving process; absent until the first round after a
    /// restart.
    pub reconciliation: Option<ReconciliationRoundReport>,
    /// Active reconciliation blocks in `block_key` order. This service always sends it; it is
    /// optional in the schema so clients also parse reports from servers that predate it.
    #[schema(required = false)]
    pub reconciliation_blocks: Vec<ReconciliationBlockReport>,
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

/// TDX evidence binding a nonce to the settlement public key and the flusher operators.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AttestationResponse {
    /// Settlement key identifier.
    pub keyid: String,
    /// Raw ed25519 settlement public key as lowercase hexadecimal.
    pub settlement_pubkey: String,
    /// Flusher operator of each configured chain, in ascending `chain_id` order. This service
    /// always sends it; it is optional in the schema so clients also parse responses from servers
    /// that predate it, whose report data binds no operators (an absent list reads as empty).
    #[schema(required = false)]
    pub operators: Vec<OperatorIdentity>,
    /// `sha256(nonce ‖ settlement_pubkey ‖ record_1 ‖ … ‖ record_n)` as lowercase hexadecimal,
    /// with one 32-byte record per operator in list order: `chain_id` (u64 big-endian),
    /// `operator_key_version` (u32 big-endian), and the 20 address bytes.
    pub report_data: String,
    /// Versioned dstack attestation bytes as lowercase hexadecimal.
    pub quote: String,
}

/// The key a chain's flusher signs `flush` transactions with; it needs `OPERATOR_ROLE` and gas.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct OperatorIdentity {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The chain's configured operator key derivation version.
    pub operator_key_version: u32,
    /// Operator key identifier, `operator/v{operator_key_version}`.
    pub keyid: String,
    /// Operator address as lowercase `0x`-prefixed hexadecimal.
    pub address: String,
}

impl From<&topup_adapters::attestation::AttestedOperator> for OperatorIdentity {
    fn from(operator: &topup_adapters::attestation::AttestedOperator) -> Self {
        Self {
            chain_id: operator.chain_id,
            operator_key_version: operator.key_version.get(),
            keyid: topup_core::operator_key_domain(operator.key_version),
            address: format!("{:#x}", operator.address),
        }
    }
}
