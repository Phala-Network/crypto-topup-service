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

/// Canonical chain evidence corrected while a deposit remains detected.
#[derive(Clone, Debug, PartialEq)]
pub struct CanonicalEvidence {
    /// Canonical finalized block number.
    pub block_number: u64,
    /// Canonical finalized block hash.
    pub block_hash: B256,
    /// Canonical block timestamp.
    pub block_time: DateTime<Utc>,
    /// Canonical token contract.
    pub asset_contract: Address,
    /// Canonical transfer sender.
    pub from_address: Address,
    /// Canonical transfer amount.
    pub amount_atomic: AtomicAmount,
    /// Route selected for the canonical token, when supported.
    pub route: Option<String>,
    /// Version selected for the canonical token, when supported.
    pub route_version: Option<u64>,
}

/// Valuation columns committed with a transition.
#[derive(Clone, Debug, PartialEq)]
pub struct StoredValuation {
    /// Time at which finality and prices were observed.
    pub valuation_at: DateTime<Utc>,
    /// Eight-decimal scaled price.
    pub price_scaled: u64,
    /// Stable source code, `spot` or `lock`.
    pub price_source: String,
    /// Product credit in minor units.
    pub credit_minor: MinorAmount,
    /// Raw price observations and validation result.
    pub quote: Value,
}

/// Conditional single-use rate-lock consumption.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockConsumption {
    /// Lock address row used as the rate-lock primary key.
    pub address_id: Uuid,
    /// Whether an existing consumption by this same deposit is accepted.
    pub idempotent: bool,
}

/// Additional writes atomically applied with one state transition.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TransitionEffects {
    /// Optional correction to provisional scanner evidence.
    pub canonical_evidence: Option<CanonicalEvidence>,
    /// Optional valuation columns.
    pub valuation: Option<StoredValuation>,
    /// Optional conditional rate-lock consumption.
    pub lock_consumption: Option<LockConsumption>,
}

/// Timeline and side effects written by one transition application.
#[derive(Clone, Copy, Debug)]
pub struct TransitionWrites<'a> {
    /// Evidence appended to the transition timeline.
    pub evidence: &'a Value,
    /// Structured deposit and lock writes.
    pub effects: &'a TransitionEffects,
    /// Outbox rows inserted after the state compare-and-swap succeeds.
    pub outbox_events: &'a [OutboxEvent],
}

/// Result of the lease-token compare-and-swap.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ApplyTransitionResult {
    /// The state, timeline, and outbox writes were applied.
    Applied,
    /// The expected state or lease token no longer matched.
    Stale,
    /// Another deposit consumed the selected rate lock before this transaction.
    LockUnavailable,
}

/// Failure while validating or persisting a transition.
#[derive(Debug, thiserror::Error)]
pub enum ApplyTransitionError {
    /// The caller supplied fields inconsistent with the core transition.
    #[error("{0}")]
    InvalidInput(&'static str),
    /// PostgreSQL rejected or failed the operation.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
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
    let mut transaction = pool.begin().await?;
    let inserted = insert_deposit_in(&mut transaction, deposit).await?;
    transaction.commit().await?;
    Ok(inserted)
}

pub(crate) async fn insert_deposit_in(
    transaction: &mut Transaction<'_, Postgres>,
    deposit: &NewDeposit,
) -> Result<bool, sqlx::Error> {
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
    .execute(&mut **transaction)
    .await?;
    let inserted = result.rows_affected() == 1;
    if inserted {
        link_inserted_deposit(transaction, id).await?;
    }
    Ok(inserted)
}

async fn link_inserted_deposit(
    transaction: &mut Transaction<'_, Postgres>,
    deposit_id: Uuid,
) -> Result<(), sqlx::Error> {
    let evidence = serde_json::json!({"outcome": "advance", "source": "confirmed_flush"});
    sqlx::query(
        r#"
        WITH candidate AS (
            SELECT f.flush_id
            FROM deposits d
            JOIN flushed f ON f.address_id = d.address_id
            JOIN flushes x ON x.id = f.flush_id
            WHERE d.id = $1
              AND x.status = 'confirmed'
              AND d.asset_contract = x.token
              AND (d.block_number, d.log_index) < (f.block_number, f.log_index)
            ORDER BY f.block_number, f.log_index, f.flush_id
            LIMIT 1
        ), previous AS (
            SELECT id, state FROM deposits WHERE id = $1 FOR UPDATE
        ), updated AS (
            UPDATE deposits d
            SET flush_id = candidate.flush_id,
                state = CASE WHEN d.state = 'credited' THEN 'swept' ELSE d.state END,
                attempt = CASE WHEN d.state = 'credited' THEN 0 ELSE d.attempt END,
                lease_token = CASE WHEN d.state = 'credited' THEN NULL ELSE d.lease_token END,
                lease_until = CASE WHEN d.state = 'credited' THEN NULL ELSE d.lease_until END,
                updated_at = now()
            FROM candidate, previous
            WHERE d.id = previous.id AND d.flush_id IS NULL
            RETURNING d.id, previous.state
        )
        INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
        SELECT gen_random_uuid(), id, 'credited', 'swept', 0, $2
        FROM updated
        WHERE state = 'credited'
        "#,
    )
    .bind(deposit_id)
    .bind(evidence)
    .execute(&mut **transaction)
    .await?;
    Ok(())
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
    writes: TransitionWrites<'_>,
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

    if let Some(consumption) = writes.effects.lock_consumption
        && !crate::locks::consume(
            transaction,
            consumption.address_id,
            deposit_id,
            consumption.idempotent,
        )
        .await?
    {
        return Ok(ApplyTransitionResult::LockUnavailable);
    }

    if let Some(canonical) = &writes.effects.canonical_evidence {
        sqlx::query(
            r#"
            UPDATE deposits
            SET block_number = $2,
                block_hash = $3,
                block_time = $4,
                asset_contract = $5,
                from_address = $6,
                amount_atomic = $7::text::numeric,
                route = $8,
                route_version = $9,
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(deposit_id)
        .bind(to_i64(canonical.block_number, "deposits.block_number")?)
        .bind(b256_hex(canonical.block_hash))
        .bind(canonical.block_time)
        .bind(address_hex(canonical.asset_contract))
        .bind(address_hex(canonical.from_address))
        .bind(atomic_decimal(canonical.amount_atomic))
        .bind(&canonical.route)
        .bind(
            canonical
                .route_version
                .map(|version| to_i64(version, "deposits.route_version"))
                .transpose()?,
        )
        .execute(&mut **transaction)
        .await?;
    }

    if let Some(valuation) = &writes.effects.valuation {
        sqlx::query(
            r#"
            UPDATE deposits
            SET valuation_at = $2,
                price_scaled = $3::text::numeric,
                price_source = $4,
                credit_minor = $5::text::numeric,
                quote = $6,
                updated_at = now()
            WHERE id = $1
            "#,
        )
        .bind(deposit_id)
        .bind(valuation.valuation_at)
        .bind(valuation.price_scaled.to_string())
        .bind(&valuation.price_source)
        .bind(valuation.credit_minor.value().to_string())
        .bind(&valuation.quote)
        .execute(&mut **transaction)
        .await?;
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
        writes.evidence
    )
    .execute(&mut **transaction)
    .await?;

    for event in writes.outbox_events {
        sqlx::query!(
            r#"
            INSERT INTO outbox (id, event_type, payload, next_attempt_at)
            VALUES ($1, $2, $3, $4)
            ON CONFLICT (id) DO NOTHING
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

/// Releases a still-owned lease after an atomic rate-lock race is lost.
pub async fn release_deposit_lease(
    pool: &PgPool,
    deposit_id: Uuid,
    lease_token: Uuid,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        UPDATE deposits
        SET lease_token = NULL,
            lease_until = NULL,
            next_attempt_at = now(),
            updated_at = now()
        WHERE id = $1 AND lease_token = $2
        "#,
    )
    .bind(deposit_id)
    .bind(lease_token)
    .execute(pool)
    .await?;
    Ok(())
}
