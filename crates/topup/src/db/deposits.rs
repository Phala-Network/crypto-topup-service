use std::error::Error;
use std::fmt::{self, Display, Formatter};

use alloy_primitives::{Address, B256};
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use topup_core::deposit::{DepositState, RejectReason, Transition};
use topup_core::identity::deposit_id;
use topup_core::money::{AtomicAmount, MinorAmount};
use uuid::Uuid;

use super::types::{
    address_hex, atomic_decimal, b256_hex, parse_address, parse_atomic_decimal, parse_b256,
    parse_optional_minor_decimal, parse_optional_u64_decimal, to_i64, to_u64,
};
use super::{parse_reason, parse_state, state_code};

/// A durable deposit row.
#[derive(Clone, Debug, PartialEq)]
pub struct Deposit {
    /// Deterministic deposit identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Transfer transaction hash.
    pub tx_hash: B256,
    /// Transfer log index.
    pub log_index: u64,
    /// Finalized block number.
    pub block_number: u64,
    /// Finalized block hash.
    pub block_hash: B256,
    /// Chain block time.
    pub block_time: DateTime<Utc>,
    /// Receiving address row.
    pub address_id: Uuid,
    /// Owning account row.
    pub account_id: Uuid,
    /// Selected route name, absent for unsupported assets.
    pub route: Option<String>,
    /// Selected route version.
    pub route_version: Option<u64>,
    /// Token contract address.
    pub asset_contract: Address,
    /// Transfer sender address.
    pub from_address: Address,
    /// Atomic token amount.
    pub amount_atomic: AtomicAmount,
    /// Current domain state.
    pub state: DepositState,
    /// Terminal rejection reason.
    pub reason: Option<RejectReason>,
    /// Retry attempt within the current state.
    pub attempt: i32,
    /// Earliest next processing time.
    pub next_attempt_at: DateTime<Utc>,
    /// Current lease ownership token.
    pub lease_token: Option<Uuid>,
    /// Current lease expiry.
    pub lease_until: Option<DateTime<Utc>>,
    /// Valuation observation time.
    pub valuation_at: Option<DateTime<Utc>>,
    /// Eight-decimal scaled price integer.
    pub price_scaled: Option<u64>,
    /// Price source code.
    pub price_source: Option<String>,
    /// Product minor-unit credit.
    pub credit_minor: Option<MinorAmount>,
    /// Stored quote evidence.
    pub quote: Option<Value>,
    /// Confirmed flush covering this deposit.
    pub flush_id: Option<Uuid>,
    /// Row creation time.
    pub created_at: DateTime<Utc>,
    /// Last row update time.
    pub updated_at: DateTime<Utc>,
}

/// Values used to insert a finalized transfer as a deposit.
#[derive(Clone, Debug, PartialEq)]
pub struct NewDeposit {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Transfer transaction hash.
    pub tx_hash: B256,
    /// Transfer log index.
    pub log_index: u64,
    /// Finalized block number.
    pub block_number: u64,
    /// Finalized block hash.
    pub block_hash: B256,
    /// Chain block time.
    pub block_time: DateTime<Utc>,
    /// Receiving address row.
    pub address_id: Uuid,
    /// Owning account row.
    pub account_id: Uuid,
    /// Selected route name, absent for unsupported assets.
    pub route: Option<String>,
    /// Selected route version.
    pub route_version: Option<u64>,
    /// Token contract address.
    pub asset_contract: Address,
    /// Transfer sender address.
    pub from_address: Address,
    /// Atomic token amount.
    pub amount_atomic: AtomicAmount,
    /// Initial domain state.
    pub state: DepositState,
    /// Initial rejection reason when the asset is unsupported.
    pub reason: Option<RejectReason>,
    /// Earliest processing time.
    pub next_attempt_at: DateTime<Utc>,
}

/// A deposit returned with a newly acquired five-minute processing lease.
pub type ClaimedDeposit = Deposit;

/// State and retry fields written by a single state-machine application.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransitionUpdate {
    /// Core-validated state transition.
    pub transition: Transition,
    /// Rejection reason, required only when entering `rejected`.
    pub rejection_reason: Option<RejectReason>,
    /// Retry attempt stored on both the deposit and timeline row.
    pub attempt: i32,
    /// Earliest next processing time.
    pub next_attempt_at: DateTime<Utc>,
}

/// An outbox event committed with a state transition.
#[derive(Clone, Debug, PartialEq)]
pub struct OutboxEvent {
    /// Event identifier.
    pub id: Uuid,
    /// Stable event type.
    pub event_type: String,
    /// Event payload.
    pub payload: Value,
    /// Earliest delivery attempt.
    pub next_attempt_at: DateTime<Utc>,
}

/// Result of the lease-token compare-and-swap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyTransitionResult {
    /// The state, timeline, and outbox writes were applied.
    Applied,
    /// The expected state or lease token no longer matched.
    Stale,
}

/// Failure while validating or persisting a transition.
#[derive(Debug)]
pub enum ApplyTransitionError {
    /// The caller supplied fields inconsistent with the core transition.
    InvalidInput(&'static str),
    /// PostgreSQL rejected or failed the operation.
    Database(sqlx::Error),
}

impl Display for ApplyTransitionError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(message) => formatter.write_str(message),
            Self::Database(error) => Display::fmt(error, formatter),
        }
    }
}

impl Error for ApplyTransitionError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::InvalidInput(_) => None,
            Self::Database(error) => Some(error),
        }
    }
}

impl From<sqlx::Error> for ApplyTransitionError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

#[derive(Debug)]
struct DepositRecord {
    id: Uuid,
    chain_id: i64,
    tx_hash: String,
    log_index: i64,
    block_number: i64,
    block_hash: String,
    block_time: DateTime<Utc>,
    address_id: Uuid,
    account_id: Uuid,
    route: Option<String>,
    route_version: Option<i64>,
    asset_contract: String,
    from_address: String,
    amount_atomic: String,
    state: String,
    reason: Option<String>,
    attempt: i32,
    next_attempt_at: DateTime<Utc>,
    lease_token: Option<Uuid>,
    lease_until: Option<DateTime<Utc>>,
    valuation_at: Option<DateTime<Utc>>,
    price_scaled: Option<String>,
    price_source: Option<String>,
    credit_minor: Option<String>,
    quote: Option<Value>,
    flush_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl TryFrom<DepositRecord> for Deposit {
    type Error = sqlx::Error;

    fn try_from(record: DepositRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            id: record.id,
            chain_id: to_u64(record.chain_id, "deposits.chain_id")?,
            tx_hash: parse_b256(&record.tx_hash)?,
            log_index: to_u64(record.log_index, "deposits.log_index")?,
            block_number: to_u64(record.block_number, "deposits.block_number")?,
            block_hash: parse_b256(&record.block_hash)?,
            block_time: record.block_time,
            address_id: record.address_id,
            account_id: record.account_id,
            route: record.route,
            route_version: record
                .route_version
                .map(|value| to_u64(value, "deposits.route_version"))
                .transpose()?,
            asset_contract: parse_address(&record.asset_contract)?,
            from_address: parse_address(&record.from_address)?,
            amount_atomic: parse_atomic_decimal(&record.amount_atomic)?,
            state: parse_state(&record.state)?,
            reason: parse_reason(record.reason.as_deref())?,
            attempt: record.attempt,
            next_attempt_at: record.next_attempt_at,
            lease_token: record.lease_token,
            lease_until: record.lease_until,
            valuation_at: record.valuation_at,
            price_scaled: parse_optional_u64_decimal(record.price_scaled.as_deref())?,
            price_source: record.price_source,
            credit_minor: parse_optional_minor_decimal(record.credit_minor.as_deref())?,
            quote: record.quote,
            flush_id: record.flush_id,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

/// Inserts a deposit and returns `false` when the chain event already exists.
pub async fn insert_deposit(pool: &PgPool, deposit: &NewDeposit) -> Result<bool, sqlx::Error> {
    let id = deposit_id(deposit.chain_id, deposit.tx_hash, deposit.log_index);
    let chain_id = to_i64(deposit.chain_id, "deposits.chain_id")?;
    let tx_hash = b256_hex(deposit.tx_hash);
    let log_index = to_i64(deposit.log_index, "deposits.log_index")?;
    let block_number = to_i64(deposit.block_number, "deposits.block_number")?;
    let block_hash = b256_hex(deposit.block_hash);
    let route_version = deposit
        .route_version
        .map(|value| to_i64(value, "deposits.route_version"))
        .transpose()?;
    let asset_contract = address_hex(deposit.asset_contract);
    let from_address = address_hex(deposit.from_address);
    let amount_atomic = atomic_decimal(deposit.amount_atomic);
    let state = state_code(deposit.state);
    let reason = deposit.reason.map(RejectReason::code);
    let result = sqlx::query!(
        r#"
        INSERT INTO deposits (
            id, chain_id, tx_hash, log_index, block_number, block_hash, block_time,
            address_id, account_id, route, route_version, asset_contract, from_address,
            amount_atomic, state, reason, next_attempt_at
        )
        VALUES (
            $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13,
            $14::text::numeric, $15, $16, $17
        )
        ON CONFLICT (chain_id, tx_hash, log_index) DO NOTHING
        "#,
        id,
        chain_id,
        tx_hash,
        log_index,
        block_number,
        block_hash,
        deposit.block_time,
        deposit.address_id,
        deposit.account_id,
        deposit.route,
        route_version,
        asset_contract,
        from_address,
        amount_atomic,
        state,
        reason,
        deposit.next_attempt_at
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Fetches a deposit by its deterministic identifier.
pub async fn get_deposit(pool: &PgPool, id: Uuid) -> Result<Option<Deposit>, sqlx::Error> {
    let record = sqlx::query_as!(
        DepositRecord,
        r#"
        SELECT
            id, chain_id, tx_hash, log_index, block_number, block_hash, block_time, address_id,
            account_id, route, route_version, asset_contract, from_address,
            amount_atomic::text AS "amount_atomic!", state, reason, attempt, next_attempt_at,
            lease_token, lease_until, valuation_at, price_scaled::text AS price_scaled,
            price_source, credit_minor::text AS credit_minor, quote, flush_id, created_at, updated_at
        FROM deposits
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(pool)
    .await?;
    record.map(TryInto::try_into).transpose()
}

/// Replaces local pricing fields with the product's authoritative original settlement inputs.
pub async fn adopt_settlement_pricing(
    pool: &PgPool,
    id: Uuid,
    credit_minor: u64,
    price_scaled: u64,
    valuation_at: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE deposits
        SET credit_minor = $2::text::numeric,
            price_scaled = $3::text::numeric,
            valuation_at = $4,
            updated_at = now()
        WHERE id = $1
        RETURNING id
        "#,
    )
    .bind(id)
    .bind(credit_minor.to_string())
    .bind(price_scaled.to_string())
    .bind(valuation_at)
    .fetch_one(pool)
    .await?;
    Ok(())
}

/// Claims one due non-terminal deposit with a five-minute lease.
pub async fn claim_deposit(
    pool: &PgPool,
    lease_token: Uuid,
) -> Result<Option<ClaimedDeposit>, sqlx::Error> {
    let record = sqlx::query_as!(
        DepositRecord,
        r#"
        WITH candidate AS (
            SELECT id
            FROM deposits
            WHERE state NOT IN ('swept', 'rejected')
              AND next_attempt_at <= now()
              AND (lease_until IS NULL OR lease_until <= now())
            ORDER BY next_attempt_at, created_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE deposits AS deposit
        SET lease_token = $1,
            lease_until = now() + interval '5 minutes',
            updated_at = now()
        FROM candidate
        WHERE deposit.id = candidate.id
        RETURNING
            deposit.id, deposit.chain_id, deposit.tx_hash, deposit.log_index,
            deposit.block_number, deposit.block_hash, deposit.block_time, deposit.address_id,
            deposit.account_id, deposit.route, deposit.route_version, deposit.asset_contract,
            deposit.from_address, deposit.amount_atomic::text AS "amount_atomic!",
            deposit.state, deposit.reason, deposit.attempt, deposit.next_attempt_at,
            deposit.lease_token, deposit.lease_until, deposit.valuation_at,
            deposit.price_scaled::text AS price_scaled, deposit.price_source,
            deposit.credit_minor::text AS credit_minor, deposit.quote, deposit.flush_id,
            deposit.created_at, deposit.updated_at
        "#,
        lease_token
    )
    .fetch_optional(pool)
    .await?;
    record.map(TryInto::try_into).transpose()
}

/// Applies a lease-token CAS and writes its timeline plus outbox events in the same transaction.
pub async fn apply_transition(
    transaction: &mut Transaction<'_, Postgres>,
    deposit_id: Uuid,
    expected_state: DepositState,
    lease_token: Uuid,
    update: TransitionUpdate,
    evidence: &Value,
    outbox_events: &[OutboxEvent],
) -> Result<ApplyTransitionResult, ApplyTransitionError> {
    if update.transition.from != expected_state {
        return Err(ApplyTransitionError::InvalidInput(
            "expected state must match the transition source",
        ));
    }
    if update.attempt < 0 {
        return Err(ApplyTransitionError::InvalidInput(
            "transition attempt cannot be negative",
        ));
    }
    let enters_rejected = update.transition.to == DepositState::Rejected;
    if enters_rejected != update.rejection_reason.is_some() {
        return Err(ApplyTransitionError::InvalidInput(
            "a rejection reason is required only when entering rejected",
        ));
    }

    let expected = state_code(expected_state);
    let target = state_code(update.transition.to);
    let reason = update.rejection_reason.map(RejectReason::code);
    let matched = sqlx::query_scalar!(
        r#"
        UPDATE deposits
        SET state = $4,
            reason = $5,
            attempt = $6,
            next_attempt_at = $7,
            lease_token = NULL,
            lease_until = NULL,
            updated_at = now()
        WHERE id = $1 AND state = $2 AND lease_token = $3
        RETURNING id
        "#,
        deposit_id,
        expected,
        lease_token,
        target,
        reason,
        update.attempt,
        update.next_attempt_at
    )
    .fetch_optional(&mut **transaction)
    .await?;

    if matched.is_none() {
        return Ok(ApplyTransitionResult::Stale);
    }

    sqlx::query!(
        r#"
        INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
        VALUES ($1, $2, $3, $4, $5, $6)
        "#,
        Uuid::new_v4(),
        deposit_id,
        expected,
        target,
        update.attempt,
        evidence
    )
    .execute(&mut **transaction)
    .await?;

    for event in outbox_events {
        sqlx::query!(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at)
            VALUES ($1, $2, $3, $4)
            "#,
            event.id,
            event.event_type,
            event.payload,
            event.next_attempt_at
        )
        .execute(&mut **transaction)
        .await?;
    }

    Ok(ApplyTransitionResult::Applied)
}
