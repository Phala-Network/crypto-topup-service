use std::error::Error;
use std::fmt::{self, Display, Formatter};

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use topup_core::deposit::{DepositState, RejectReason, Transition};
use uuid::Uuid;

use super::{parse_reason, parse_state, state_code};

/// A durable deposit row.
#[derive(Clone, Debug, PartialEq)]
pub struct Deposit {
    /// Deterministic deposit identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: i64,
    /// Transfer transaction hash.
    pub tx_hash: String,
    /// Transfer log index.
    pub log_index: i64,
    /// Finalized block number.
    pub block_number: i64,
    /// Finalized block hash.
    pub block_hash: String,
    /// Chain block time.
    pub block_time: DateTime<Utc>,
    /// Receiving address row.
    pub address_id: Uuid,
    /// Owning account row.
    pub account_id: Uuid,
    /// Selected route name, absent for unsupported assets.
    pub route: Option<String>,
    /// Selected route version.
    pub route_version: Option<i64>,
    /// Token contract address.
    pub asset_contract: String,
    /// Transfer sender address.
    pub from_address: String,
    /// Atomic token amount as an unsigned decimal string.
    pub amount_atomic: String,
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
    /// Eight-decimal scaled price as an unsigned decimal string.
    pub price_scaled: Option<String>,
    /// Price source code.
    pub price_source: Option<String>,
    /// Product minor-unit credit as an unsigned decimal string.
    pub credit_minor: Option<String>,
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
    /// Deterministic deposit identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: i64,
    /// Transfer transaction hash.
    pub tx_hash: String,
    /// Transfer log index.
    pub log_index: i64,
    /// Finalized block number.
    pub block_number: i64,
    /// Finalized block hash.
    pub block_hash: String,
    /// Chain block time.
    pub block_time: DateTime<Utc>,
    /// Receiving address row.
    pub address_id: Uuid,
    /// Owning account row.
    pub account_id: Uuid,
    /// Selected route name, absent for unsupported assets.
    pub route: Option<String>,
    /// Selected route version.
    pub route_version: Option<i64>,
    /// Token contract address.
    pub asset_contract: String,
    /// Transfer sender address.
    pub from_address: String,
    /// Atomic token amount as an unsigned decimal string.
    pub amount_atomic: String,
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
            chain_id: record.chain_id,
            tx_hash: record.tx_hash,
            log_index: record.log_index,
            block_number: record.block_number,
            block_hash: record.block_hash,
            block_time: record.block_time,
            address_id: record.address_id,
            account_id: record.account_id,
            route: record.route,
            route_version: record.route_version,
            asset_contract: record.asset_contract,
            from_address: record.from_address,
            amount_atomic: record.amount_atomic,
            state: parse_state(&record.state)?,
            reason: parse_reason(record.reason.as_deref())?,
            attempt: record.attempt,
            next_attempt_at: record.next_attempt_at,
            lease_token: record.lease_token,
            lease_until: record.lease_until,
            valuation_at: record.valuation_at,
            price_scaled: record.price_scaled,
            price_source: record.price_source,
            credit_minor: record.credit_minor,
            quote: record.quote,
            flush_id: record.flush_id,
            created_at: record.created_at,
            updated_at: record.updated_at,
        })
    }
}

/// Inserts a deposit and returns `false` when the chain event already exists.
pub async fn insert_deposit(pool: &PgPool, deposit: &NewDeposit) -> Result<bool, sqlx::Error> {
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
        deposit.id,
        deposit.chain_id,
        deposit.tx_hash,
        deposit.log_index,
        deposit.block_number,
        deposit.block_hash,
        deposit.block_time,
        deposit.address_id,
        deposit.account_id,
        deposit.route,
        deposit.route_version,
        deposit.asset_contract,
        deposit.from_address,
        deposit.amount_atomic,
        state,
        reason,
        deposit.next_attempt_at
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
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
