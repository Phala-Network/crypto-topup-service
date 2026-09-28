//! Typed HTTP request and response models.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use uuid::Uuid;

/// A deposit's stored facts, for the operator (`GET /v1/admin/deposits/{id}`).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositResponse {
    /// Deposit id, `dep_…`.
    pub id: String,
    /// The merchant account the deposit belongs to, `acct_…`. Optional in the schema so clients
    /// also parse responses from servers that predate accounts.
    #[schema(required = false)]
    pub account: String,
    /// Whether the deposit is on a live route. Optional in the schema, like `account`.
    #[schema(required = false)]
    pub livemode: bool,
    /// The merchant's identifier of the customer the quote was issued for. This service always
    /// sends it; it is optional in the schema so clients also parse responses from servers that
    /// predate it.
    #[schema(required = false)]
    pub external_id: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Canonical transaction hash.
    pub tx_hash: String,
    /// Position of the transfer log in its transaction's receipt; with the chain and transaction,
    /// the deposit's identity. Optional in the schema so clients also parse responses from
    /// servers that predate it.
    #[schema(required = false)]
    pub receipt_log_index: u64,
    /// Block-wide transfer log index; it follows the transaction's re-inclusion.
    pub log_index: u64,
    /// Including block number.
    pub block_number: u64,
    /// Including block time.
    pub block_time: DateTime<Utc>,
    /// When both providers showed the transfer at or below `finalized`; `null` while the deposit
    /// can still be reversed.
    pub final_at: Option<DateTime<Utc>>,
    /// Receiving forwarder address.
    pub address: String,
    /// The quote whose address received the deposit, `qt_…`; `null` for a deposit address.
    pub lock_ref: Option<String>,
    /// The deposit address that received the deposit, `da_…`; `null` for a quote's address.
    #[schema(required = false)]
    pub deposit_address: Option<String>,
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
    /// Credit in minor units encoded as a decimal string.
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
    /// The deposit's stored facts.
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
    /// Stable event identifier, sent as the `webhook-id` header: `evt_…`.
    pub id: String,
    /// Event type, such as `deposit.credited`.
    pub event_type: String,
    /// Event creation time.
    pub created_at: DateTime<Utc>,
    /// When the last of the account's webhook endpoints accepted the event, or `null` while one
    /// has not or the account has none.
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
    /// Your identifier of the customer account to credit, 1 to 200 characters; the account is
    /// created on its first quote.
    pub account_id: String,
    /// The credit to quote, a positive integer in the currency's minor unit (US cents).
    pub amount: u64,
    /// Lowercase ISO currency code; only `usd`.
    pub currency: String,
    /// EVM chain of the payment, one of `GET /v1/config` `assets[].chain_id`.
    pub chain_id: u64,
    /// Asset code of the payment on that chain, such as `pha`.
    pub asset: String,
    /// Stripe's `metadata`: up to 50 string key/value pairs for your own use, keys of up to 40
    /// characters without square brackets, values of up to 500 characters.
    /// A key set to `""` is omitted. The deposit that pays the quote starts with a copy of it.
    /// Phala Pay never reads it. Do not store sensitive information in it, such as personal or
    /// payment details.
    #[serde(default, deserialize_with = "super::metadata::present")]
    #[schema(value_type = MetadataParam, required = false)]
    pub metadata: Option<serde_json::Value>,
}

/// `POST /v1/quotes/{id}`, `POST /v1/deposits/{id}`, `POST /v1/refunds/{id}`, and
/// `POST /v1/deposit_addresses/{id}` body: the
/// object's updatable parameters, of which `metadata` is the one.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateMetadataRequest {
    /// Stripe's `metadata`: up to 50 string key/value pairs for your own use, keys of up to 40
    /// characters without square brackets, values of up to 500 characters.
    /// Merged into the object's: a key set to a value is set, a key set to `""` is unset, other
    /// keys are kept, and `metadata: ""` unsets every key.
    /// Phala Pay never reads it. Do not store sensitive information in it, such as personal or
    /// payment details.
    #[serde(default, deserialize_with = "super::metadata::present")]
    #[schema(value_type = MetadataParam, required = false)]
    pub metadata: Option<serde_json::Value>,
}

/// A `metadata` parameter: an object of string values, where `""` unsets the key, or `""` to
/// unset every key.
#[derive(Serialize, ToSchema)]
#[serde(untagged)]
#[allow(dead_code)]
pub enum MetadataParam {
    /// Keys to set, or with `""` to unset.
    Pairs(std::collections::BTreeMap<String, String>),
    /// `""`: unset every key.
    Clear(MetadataClear),
}

/// `""`: unset every key of the object's metadata.
#[derive(Serialize)]
pub struct MetadataClear;

impl utoipa::PartialSchema for MetadataClear {
    fn schema() -> utoipa::openapi::RefOr<utoipa::openapi::schema::Schema> {
        utoipa::openapi::ObjectBuilder::new()
            .schema_type(utoipa::openapi::schema::Type::String)
            .enum_values(Some([""]))
            .description(Some("`\"\"`: unset every key of the object's metadata."))
            .into()
    }
}

impl ToSchema for MetadataClear {}

/// A quote: a locked price, an exact token amount, and a single-use address to pay it to.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Quote {
    /// `qt_` id. The quote's address salt is `keccak256(abi.encode(account, account_id, "lock",
    /// id))`, where `account` is your `acct_` id.
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
    /// Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}` when none.
    /// Always sent; optional in the schema so clients also parse objects from servers, and
    /// events rendered, before metadata.
    #[schema(required = false)]
    pub metadata: std::collections::BTreeMap<String, String>,
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
    /// anything: `none`; `seen` (in a block, below the route's confirmation, and may still
    /// disappear); `confirming` (at the route's confirmation, being valued and screened);
    /// `credited`; or `rejected` (not credited; the payer should contact the product's support).
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
    Quote(Box<Quote>),
    /// The payer's view.
    Client(Box<ClientQuote>),
}

/// A payment observed at a quote's address. Display only: while `status` is `seen` it is not
/// final, may still disappear in a reorg, and nothing has been credited.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct QuotePayment {
    /// `seen` (in a block, not recorded as a deposit yet) or `final` (recorded as a deposit at the
    /// route's confirmation; it is final once its block is). New values may be added.
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
    /// `dep_` id the deposit has, or will have once recorded.
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

/// A transfer to a quote's address or a deposit address at the route's confirmation: valued,
/// screened, and credited, or rejected; `reversed` if its transaction left the chain before
/// finality.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct Deposit {
    /// `dep_` and the hex of the deposit's deterministic UUID,
    /// `uuid_v5(DEPOSIT_NAMESPACE, "{chain_id}:{tx_hash}:{receipt_log_index}")`, where
    /// `receipt_log_index` is the transfer's position among its transaction's receipt logs.
    pub id: String,
    /// Always `deposit`.
    pub object: String,
    /// Your account identifier.
    pub account_id: String,
    /// The quote whose address received the transfer; `null` for a deposit address.
    pub quote: Option<ExpandableQuote>,
    /// The deposit address that received the transfer, `da_…`; `null` for a quote's address.
    /// Payments to a deposit address, active or retired, are credited at spot. This service
    /// always sends it; it is optional in the schema so clients also parse responses and events
    /// from servers that predate it.
    #[schema(required = false)]
    pub deposit_address: Option<String>,
    /// `detected`, `confirmed`, `credited`, `swept`, `rejected`, or `reversed` (the transaction is
    /// not in the final chain: claw back a credit as for `deposit.refunded`). New values may be
    /// added.
    pub status: String,
    /// Why the deposit was rejected: `unsupported_asset`, `below_minimum`, `out_of_bounds`,
    /// `out_of_range`, or `sanctioned`.
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
    /// Block-wide log index of the transfer; it changes if the transaction is re-included.
    pub log_index: u64,
    /// Number of the block the transfer is in; it changes if the transaction is re-included.
    pub block_number: u64,
    /// Refunded token amount in base units, as a decimal string: the sum of succeeded refunds.
    pub amount_refunded_atomic: String,
    /// Whether the deposit is fully refunded.
    pub refunded: bool,
    /// Detection time, Unix seconds.
    pub created: i64,
    /// Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)): a copy of the
    /// quote's when the deposit is recorded, independent of it afterwards; `{}` when none.
    /// Always sent; optional in the schema like the quote's.
    #[schema(required = false)]
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// `POST /v1/deposit_addresses` body.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateDepositAddressRequest {
    /// Your identifier of the customer, 1 to 200 characters; the customer is created on first use.
    pub client_reference_id: String,
    /// EVM chain, one of `GET /v1/config` `assets[].chain_id`.
    pub chain_id: u64,
    /// Asset code on that chain, such as `pha`.
    pub asset: String,
    /// Stripe's `metadata`: up to 50 string key/value pairs for your own use, keys of up to 40
    /// characters without square brackets, values of up to 500 characters.
    /// Merged into the returned address's, as `POST /v1/deposit_addresses/{id}` does: a key set to
    /// `""` is unset. Every deposit to the address starts with a copy of it, and a rotation carries
    /// it to the next version. Phala Pay never reads it. Do not store sensitive information in it,
    /// such as personal or payment details.
    #[serde(default, deserialize_with = "super::metadata::present")]
    #[schema(value_type = MetadataParam, required = false)]
    pub metadata: Option<serde_json::Value>,
}

/// A customer's persistent deposit address for one chain and asset, like a bank-transfer virtual
/// account: any amount sent to it is credited to the customer at the market (spot) price when it
/// arrives. Rotation retires it and issues a new one; a retired address is still credited.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositAddress {
    /// `da_` id.
    pub id: String,
    /// Always `deposit_address`.
    pub object: String,
    /// Whether the address is in live mode.
    pub livemode: bool,
    /// Your identifier of the customer.
    pub client_reference_id: String,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Asset code.
    pub asset: String,
    /// The forwarder address to pay.
    pub address: String,
    /// EIP-681 ERC-20 transfer URI carrying the token, chain, and address, and no amount: the
    /// payer chooses it.
    pub payment_uri: String,
    /// The treasury the forwarder pays, fixed when the address was issued.
    pub treasury: String,
    /// The address's version among the customer's addresses for this chain and asset, from 1.
    /// The salt is `keccak256(abi.encode(account, livemode, client_reference_id,
    /// "deposit_address", chain_id, asset, version))`, with the types `(string, bool, string,
    /// string, uint256, string, uint256)` and `account` your `acct_` id; the address is the
    /// factory's `CREATE2` over the treasury and that salt.
    pub version: u64,
    /// CREATE2 salt, 32 bytes of hex.
    pub salt: String,
    /// `active`, or `retired` by a rotation; payments to either are credited.
    pub status: String,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// Retirement time, Unix seconds; `null` while active.
    pub retired_at: Option<i64>,
    /// Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}` when none.
    /// Each deposit to the address starts with a copy.
    pub metadata: std::collections::BTreeMap<String, String>,
}

/// A page of deposit addresses, newest first (<https://docs.stripe.com/api/pagination>).
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct DepositAddressList {
    /// Always `list`.
    pub object: String,
    /// The list's path, `/v1/deposit_addresses`.
    pub url: String,
    /// Whether more addresses follow in the direction of this page.
    pub has_more: bool,
    /// The deposit addresses.
    pub data: Vec<DepositAddress>,
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
    /// Stripe's `metadata`: up to 50 string key/value pairs for your own use, keys of up to 40
    /// characters without square brackets, values of up to 500 characters.
    /// A key set to `""` is omitted.
    /// Phala Pay never reads it. Do not store sensitive information in it, such as personal or
    /// payment details.
    #[serde(default, deserialize_with = "super::metadata::present")]
    #[schema(value_type = MetadataParam, required = false)]
    pub metadata: Option<serde_json::Value>,
}

/// `POST /v1/refunds/{id}/mark_paid` body: the merchant's refund transaction.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MarkRefundPaidRequest {
    /// Hash of the transaction that pays the refund from the treasury of the deposit's address.
    pub transaction_hash: String,
    /// Block-wide index of the `Transfer` log that pays the refund; any matching log when absent.
    pub log_index: Option<u64>,
}

/// A refund of (part of) a deposit to the customer, which the merchant pays from the treasury of
/// the deposit's address and attaches with `mark_paid` (design D5).
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
    /// The treasury the refund must be paid from: the one the deposit's address pays, which may
    /// differ from the account's current treasury.
    pub treasury: String,
    /// `pending` (awaiting payment, or its transaction's finality), `succeeded` (the transfer is
    /// final), `failed` (the attached transaction does not pay the refund; see
    /// `failure_reason`), or `canceled`.
    pub status: String,
    /// Why the refund failed: `transaction_failed`, `transfer_not_found`, `sender_mismatch`,
    /// `destination_mismatch`, `amount_mismatch`, or `transfer_already_used`. New values may be
    /// added.
    pub failure_reason: Option<String>,
    /// The attached refund transaction, once marked paid.
    pub transaction_hash: Option<String>,
    /// Block-wide index of the paying `Transfer` log: as named when marked paid, or found at
    /// verification.
    pub log_index: Option<u64>,
    /// Request time, Unix seconds.
    pub created: i64,
    /// Your key/value pairs ([metadata](https://docs.stripe.com/api/metadata)); `{}` when none.
    /// Always sent; optional in the schema so clients also parse objects from servers, and
    /// events rendered, before metadata.
    #[schema(required = false)]
    pub metadata: std::collections::BTreeMap<String, String>,
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
    /// The confirmation a payment's block must reach before it is credited: a depth (`"2"`: the
    /// block and one more), `"safe"`, or `"finalized"`. A credit before finality can still be
    /// reversed (`deposit.reversed`). Optional in the schema, like `typical_credit_seconds`, so
    /// clients also parse responses from servers that predate fast credit.
    #[schema(required = false)]
    pub confirmations: String,
    /// Typical time from payment to the `deposit.credited` event, in seconds.
    #[schema(required = false)]
    pub typical_credit_seconds: u64,
    /// Typical time from payment to finality, in seconds; refunds wait for it.
    pub typical_finality_seconds: u64,
}

/// The merchant's contact recorded at onboarding (design D8): the operator's channel for the key
/// hand-over, recovery, incidents, and restores, and the only personal data kept.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Contact {
    /// The contact's name, 1 to 200 characters.
    pub name: String,
    /// The security contact's email address.
    pub email: String,
}

/// The record of the operator's offline due diligence (design D8): a reference to it, when, and
/// by whom.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DueDiligence {
    /// Reference to the review in Phala's records, 1 to 200 characters.
    pub reference: String,
    /// Date of the review, `YYYY-MM-DD`.
    #[schema(value_type = String, format = Date)]
    pub reviewed_at: chrono::NaiveDate,
    /// Who reviewed, 1 to 200 characters.
    pub reviewed_by: String,
}

/// `POST /v1/admin/accounts` body. Accounts are created only by the operator (design D8).
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateAccountRequest {
    /// Display name, 1 to 200 characters.
    pub name: String,
    /// The merchant's contact.
    pub contact: Contact,
    /// The due diligence the decision rests on.
    pub due_diligence: DueDiligence,
    /// Whether the account may use live mode (design D12). Default `false`.
    #[serde(default)]
    pub charges_enabled: bool,
    /// Why the account is created, 1 to 1024 bytes.
    pub reason: String,
    /// Absolute `https` URL of the account's webhook receiver, registered in each enabled mode;
    /// `http` only when the service's own public origin uses `http` (local stacks). Until
    /// merchants register endpoints through the API (design PR 8).
    #[serde(default)]
    pub webhook_url: Option<String>,
}

/// `POST /v1/admin/accounts/{account}` body; absent fields stay as they are.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct UpdateAccountRequest {
    /// Enables or disables live mode. Enabling it for an account without a live key returns the
    /// account's first live key.
    #[serde(default)]
    pub charges_enabled: Option<bool>,
    /// Marks the account restricted for review.
    #[serde(default)]
    pub restricted: Option<bool>,
    /// Replaces the merchant's contact.
    #[serde(default)]
    pub contact: Option<Contact>,
    /// Replaces the URL of the account's webhook endpoints (until design PR 8).
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Why, 1 to 1024 bytes.
    pub reason: String,
}

/// An account as the operator sees it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AccountResponse {
    /// Account id, `acct_…`.
    pub id: String,
    /// Always `account`.
    pub object: String,
    /// Display name.
    pub name: String,
    /// The merchant's contact.
    pub contact: Contact,
    /// The due diligence record.
    pub due_diligence: DueDiligence,
    /// Whether the account may use live mode.
    pub charges_enabled: bool,
    /// Whether the account is restricted for review.
    pub restricted: bool,
    /// Active account-level pause scopes.
    pub paused_scopes: Vec<String>,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// The secret keys this request issued, each with its `secret` shown only here: at creation
    /// a test key and, with `charges_enabled`, a live key; on an update that enables live mode,
    /// the first live key. Send them to the contact; the merchant rolls them on receipt.
    pub api_keys: Vec<ApiKeyObject>,
}

/// `POST /v1/admin/accounts/{account}/api_keys` body: a recovery key (design D7).
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueApiKeyRequest {
    /// The key's mode; `true` needs `charges_enabled`.
    pub livemode: bool,
    /// Revokes every key of the mode first, for a leak the merchant cannot win by rolling.
    #[serde(default)]
    pub revoke_existing: bool,
    /// The key's label, at most 200 characters.
    #[serde(default)]
    pub name: String,
    /// Why, 1 to 1024 bytes: how the request was verified with the recorded contact.
    pub reason: String,
}

/// The account of the request's API key (`GET /v1/account`), in the key's mode.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AccountObject {
    /// Account id, `acct_…`.
    pub id: String,
    /// Always `account`.
    pub object: String,
    /// The mode of the key that reads it.
    pub livemode: bool,
    /// Display name.
    pub name: String,
    /// Whether the operator enabled live mode.
    pub charges_enabled: bool,
    /// Active account-level pause scopes.
    pub paused_scopes: Vec<String>,
    /// Creation time, Unix seconds.
    pub created: i64,
}

/// Administrative pause or resume of one customer of an account.
#[derive(Clone, Debug, Deserialize, ToSchema)]
pub struct CustomerPauseRequest {
    /// Pause scopes to add or remove.
    pub scopes: Vec<String>,
    /// The mode of the customer: customers of test and live mode are separate.
    pub livemode: bool,
}

/// An API key (design D7). `secret` is present only in the response that created it.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ApiKeyObject {
    /// Key id, `key_…`.
    pub id: String,
    /// Always `api_key`.
    pub object: String,
    /// The key's mode.
    pub livemode: bool,
    /// `secret`; `restricted` keys come later.
    #[serde(rename = "type")]
    pub key_type: String,
    /// The key's label.
    pub name: String,
    /// The whole key, `ppay_sk_…`, shown once. Store it in a secret manager.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret: Option<String>,
    /// The key's prefix and last four characters, such as `ppay_sk_test_…a1B2`.
    pub redacted: String,
    /// `active`; `expiring` for a rolled key that still works until `expires_at`; `expired`;
    /// `revoked`.
    pub status: String,
    /// Creation time, Unix seconds.
    pub created: i64,
    /// When a rolled key stops working, Unix seconds.
    pub expires_at: Option<i64>,
    /// Last use, Unix seconds, to the minute.
    pub last_used: Option<i64>,
}

/// `GET /v1/api_keys` response.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct ApiKeyList {
    /// Always `list`.
    pub object: String,
    /// The list's path, `/v1/api_keys`.
    pub url: String,
    /// Always `false`: every key of the mode is listed.
    pub has_more: bool,
    /// The mode's keys, newest first.
    pub data: Vec<ApiKeyObject>,
}

/// `POST /v1/api_keys` body.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct CreateApiKeyRequest {
    /// The key's label, at most 200 characters.
    #[serde(default)]
    pub name: String,
}

/// `POST /v1/api_keys/{id}/roll` body.
#[derive(Clone, Debug, Default, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RollApiKeyRequest {
    /// Seconds the old key keeps working, up to 604800 (7 days); 0, the default, revokes it at
    /// once.
    #[serde(default)]
    pub expires_in: u32,
}

/// Administrative deposit nudge result.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct NudgeResponse {
    /// Nudged deposit id, `dep_…`.
    pub deposit_id: String,
    /// Newly due processing time.
    pub next_attempt_at: DateTime<Utc>,
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
    /// Stable event identifier, sent as the `webhook-id` header: `evt_…`.
    pub event_id: String,
    /// Event type, such as `deposit.credited`.
    pub event_type: String,
    /// When the delivery worker next attempts the event, or `null` when the account has no
    /// webhook endpoint to deliver it to.
    pub next_attempt_at: Option<DateTime<Utc>>,
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
    /// Deposits not reversed minus finalized `Flushed` amounts: what the route's forwarders
    /// still hold for their merchants to sweep.
    pub unflushed_balance_atomic: String,
    /// Sum of unconsumed rate-lock token amounts.
    pub open_rate_lock_exposure_atomic: String,
    /// Rejected token amount still held after succeeded refunds.
    pub rejected_holds_atomic: String,
    /// Deposit counts keyed by state.
    pub deposits_by_state: std::collections::BTreeMap<String, u64>,
    /// Credited deposits whose `deposit.credited` webhook an endpoint has not acknowledged yet.
    pub credited_undelivered: u64,
    /// Age in seconds of the oldest of those events; zero when every one was delivered.
    pub credited_undelivered_max_age_seconds: u64,
    /// Refund counts keyed by status.
    pub refunds_by_status: std::collections::BTreeMap<String, u64>,
    /// Maximum age in seconds keyed by current deposit state.
    pub age_in_state_max_seconds: std::collections::BTreeMap<String, u64>,
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
    /// Block identifier, `chain:{chain_id}`.
    pub block_key: String,
    /// `chain`: the chain is frozen. No check writes the `address` scope any more; it excluded an
    /// address from the removed operator flusher.
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
    /// Open rate-lock credit across all accounts in destination minor units: the sum the global
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

/// TDX evidence binding a nonce to the settlement public key.
#[derive(Clone, Debug, Serialize, ToSchema)]
pub struct AttestationResponse {
    /// Settlement key identifier.
    pub keyid: String,
    /// Raw ed25519 settlement public key as lowercase hexadecimal.
    pub settlement_pubkey: String,
    /// `sha256(nonce ‖ settlement_pubkey)` as lowercase hexadecimal.
    pub report_data: String,
    /// Versioned dstack attestation bytes as lowercase hexadecimal.
    pub quote: String,
}
