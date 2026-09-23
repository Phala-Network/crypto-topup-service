use alloy_primitives::{Address, B256};
use chrono::{DateTime, Utc};
use serde_json::{Value, json};
use sqlx::{PgPool, Postgres, Row, Transaction};
use topup_core::money::AtomicAmount;
use uuid::Uuid;

use super::types::{
    address_hex, atomic_decimal, b256_hex, parse_address, parse_b256, to_i64, to_u64,
};

/// Durable status of a factory flush transaction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlushStatus {
    /// The salts and nonce are durably reserved but no transaction is signed.
    Planned,
    /// A signed raw transaction is durably stored and may be in the mempool.
    Sent,
    /// The successful receipt is finalized and its events are persisted.
    Confirmed,
    /// The transaction receipt has a failed status.
    Reverted,
}

impl FlushStatus {
    fn code(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Sent => "sent",
            Self::Confirmed => "confirmed",
            Self::Reverted => "reverted",
        }
    }

    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "planned" => Ok(Self::Planned),
            "sent" => Ok(Self::Sent),
            "confirmed" => Ok(Self::Confirmed),
            "reverted" => Ok(Self::Reverted),
            other => Err(sqlx::Error::Decode(
                format!("unknown flush status `{other}`").into(),
            )),
        }
    }
}

/// A persisted factory flush transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct Flush {
    /// Flush identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Token contract address.
    pub token: Address,
    /// Operator address.
    pub operator: Address,
    /// Operator transaction nonce.
    pub nonce: u64,
    /// Most recently signed transaction hash.
    pub tx_hash: Option<B256>,
    /// Finalized block number when confirmed or reverted.
    pub block_number: Option<u64>,
    /// Durable transaction state.
    pub status: FlushStatus,
    /// Plan, signed transaction history, and receipt evidence.
    pub receipt: Value,
}

/// Values used to persist a planned or observed flush transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFlush {
    /// Flush identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: u64,
    /// Token contract address.
    pub token: Address,
    /// Operator address.
    pub operator: Address,
    /// Operator transaction nonce.
    pub nonce: u64,
    /// Transaction hash when sent.
    pub tx_hash: Option<B256>,
    /// Finalized block number when confirmed.
    pub block_number: Option<u64>,
    /// Flush status code.
    pub status: String,
    /// Transaction receipt when available.
    pub receipt: Option<Value>,
}

/// One confirmed `Flushed` contract event.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlushedEvent {
    /// Owning flush transaction.
    pub flush_id: Uuid,
    /// Address emptied by the event.
    pub address_id: Uuid,
    /// Atomic token amount.
    pub amount_atomic: AtomicAmount,
    /// Event block number.
    pub block_number: u64,
    /// Event log index.
    pub log_index: u64,
}

/// Inserts a flush transaction row.
pub async fn insert_flush(pool: &PgPool, flush: &NewFlush) -> Result<(), sqlx::Error> {
    let chain_id = to_i64(flush.chain_id, "flushes.chain_id")?;
    let token = address_hex(flush.token);
    let operator = address_hex(flush.operator);
    let nonce = flush.nonce.to_string();
    let tx_hash = flush.tx_hash.map(b256_hex);
    let block_number = flush
        .block_number
        .map(|value| to_i64(value, "flushes.block_number"))
        .transpose()?;
    sqlx::query(
        r#"
        INSERT INTO flushes
            (id, chain_id, token, operator, nonce, tx_hash, block_number, status, receipt)
        VALUES ($1, $2, $3, $4, $5::text::numeric, $6, $7, $8, $9)
        "#,
    )
    .bind(flush.id)
    .bind(chain_id)
    .bind(token)
    .bind(operator)
    .bind(nonce)
    .bind(tx_hash)
    .bind(block_number)
    .bind(&flush.status)
    .bind(&flush.receipt)
    .execute(pool)
    .await?;
    Ok(())
}

/// Acquires the transaction-scoped nonce lock for one chain and operator.
pub async fn lock_operator(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    operator: Address,
) -> Result<(), sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    let operator = address_hex(operator);
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(operator)
        .bind(chain_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Acquires the transaction-scoped planning lock for one chain and token.
pub async fn lock_flush_plan(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    token: Address,
) -> Result<(), sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    let key = format!("flush-plan:{}", address_hex(token));
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, $2))")
        .bind(key)
        .bind(chain_id)
        .execute(&mut **transaction)
        .await?;
    Ok(())
}

/// Returns the next free nonce, considering both the pending chain nonce and reserved DB rows.
pub async fn next_flush_nonce(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    operator: Address,
    pending_nonce: u64,
) -> Result<u64, sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    let operator = address_hex(operator);
    let stored = sqlx::query_scalar::<_, String>(
        r#"
        SELECT nonce::text
        FROM flushes
        WHERE chain_id = $1 AND operator = $2
        ORDER BY nonce DESC
        LIMIT 1
        "#,
    )
    .bind(chain_id)
    .bind(operator)
    .fetch_optional(&mut **transaction)
    .await?
    .map(|value| parse_u64_decimal(&value, "flushes.nonce"))
    .transpose()?;
    let reserved = stored
        .map(|value| {
            value.checked_add(1).ok_or_else(|| {
                sqlx::Error::Decode("flush nonce exceeds u64 after increment".into())
            })
        })
        .transpose()?
        .unwrap_or(0);
    Ok(pending_nonce.max(reserved))
}

/// Inserts a planned flush while the caller holds the operator nonce lock.
pub async fn insert_planned_flush(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    chain_id: u64,
    token: Address,
    operator: Address,
    nonce: u64,
    receipt: &Value,
) -> Result<(), sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    sqlx::query(
        r#"
        INSERT INTO flushes (id, chain_id, token, operator, nonce, status, receipt)
        VALUES ($1, $2, $3, $4, $5::text::numeric, 'planned', $6)
        "#,
    )
    .bind(id)
    .bind(chain_id)
    .bind(address_hex(token))
    .bind(address_hex(operator))
    .bind(nonce.to_string())
    .bind(receipt)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Returns whether an operator currently has a sent transaction awaiting finalization.
pub async fn has_sent_flush(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    operator: Address,
) -> Result<bool, sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    let operator = address_hex(operator);
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM flushes WHERE chain_id = $1 AND operator = $2 AND status = 'sent')",
    )
    .bind(chain_id)
    .bind(operator)
    .fetch_one(&mut **transaction)
    .await
}

/// Returns whether any operator has a sent transaction for this chain and token.
pub async fn has_sent_flush_for_token(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    token: Address,
) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM flushes WHERE chain_id = $1 AND token = $2 AND status = 'sent')",
    )
    .bind(to_i64(chain_id, "flushes.chain_id")?)
    .bind(address_hex(token))
    .fetch_one(&mut **transaction)
    .await
}

/// Rebinds unsigned plans for a token to the current operator and fresh nonce sequence.
pub async fn rebind_planned_flushes(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    token: Address,
    operator: Address,
    pending_nonce: u64,
) -> Result<Vec<Flush>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, chain_id, token, operator, nonce::text AS nonce, tx_hash, block_number,
               status, COALESCE(receipt, '{}'::jsonb) AS receipt
        FROM flushes
        WHERE chain_id = $1 AND token = $2 AND status = 'planned'
        ORDER BY nonce, id
        FOR UPDATE
        "#,
    )
    .bind(to_i64(chain_id, "flushes.chain_id")?)
    .bind(address_hex(token))
    .fetch_all(&mut **transaction)
    .await?;
    let plans = rows
        .into_iter()
        .map(parse_flush_row)
        .collect::<Result<Vec<_>, _>>()?;
    if plans.is_empty() {
        return Ok(plans);
    }
    let stale = plans
        .iter()
        .filter(|plan| plan.operator != operator)
        .collect::<Vec<_>>();
    if stale.is_empty() {
        return Ok(plans);
    }
    let mut nonce = next_flush_nonce(transaction, chain_id, operator, pending_nonce).await?;
    for plan in stale {
        sqlx::query(
            "UPDATE flushes SET operator = $2, nonce = $3::text::numeric WHERE id = $1 AND status = 'planned' AND operator <> $2",
        )
        .bind(plan.id)
        .bind(address_hex(operator))
        .bind(nonce.to_string())
        .execute(&mut **transaction)
        .await?;
        sqlx::query(
            r#"
            INSERT INTO audit (id, actor, action, subject, reason)
            VALUES ($1, $2, 'flush.plan_operator_rebound', $3, $4)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(address_hex(operator))
        .bind(plan.id.to_string())
        .bind(format!(
            "rebound unsigned plan from operator {} nonce {}",
            address_hex(plan.operator),
            plan.nonce
        ))
        .execute(&mut **transaction)
        .await?;
        nonce = nonce.checked_add(1).ok_or_else(|| {
            sqlx::Error::Protocol("flush nonce overflowed while rebinding plans".to_owned())
        })?;
    }
    Ok(plans)
}

/// Lists address identifiers whose planning exclusion has not expired.
pub async fn list_active_flush_exclusions(
    pool: &PgPool,
    chain_id: u64,
    token: Address,
    now: DateTime<Utc>,
) -> Result<Vec<Uuid>, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT address_id FROM flush_exclusions WHERE chain_id = $1 AND token = $2 AND retry_after > $3",
    )
    .bind(to_i64(chain_id, "flush_exclusions.chain_id")?)
    .bind(address_hex(token))
    .bind(now)
    .fetch_all(pool)
    .await
}

/// Persists or refreshes a singleton planning exclusion.
pub async fn upsert_flush_exclusion(
    pool: &PgPool,
    chain_id: u64,
    token: Address,
    address_id: Uuid,
    reason: &str,
    retry_after: DateTime<Utc>,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO flush_exclusions (chain_id, token, address_id, reason, retry_after, failures)
        VALUES ($1, $2, $3, $4, $5, 1)
        ON CONFLICT (chain_id, token, address_id) DO UPDATE
        SET reason = EXCLUDED.reason,
            retry_after = EXCLUDED.retry_after,
            failures = flush_exclusions.failures + 1,
            updated_at = now()
        "#,
    )
    .bind(to_i64(chain_id, "flush_exclusions.chain_id")?)
    .bind(address_hex(token))
    .bind(address_id)
    .bind(reason)
    .bind(retry_after)
    .execute(pool)
    .await?;
    Ok(())
}

/// Returns whether a chain and token already has a planned or sent flush.
pub async fn has_open_flush(
    pool: &PgPool,
    chain_id: u64,
    token: Address,
) -> Result<bool, sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM flushes WHERE chain_id = $1 AND token = $2 AND status IN ('planned', 'sent'))",
    )
    .bind(chain_id)
    .bind(address_hex(token))
    .fetch_one(pool)
    .await
}

/// Returns whether a chain and token already has a planned or sent flush in this transaction.
pub async fn has_open_flush_locked(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    token: Address,
) -> Result<bool, sqlx::Error> {
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS(SELECT 1 FROM flushes WHERE chain_id = $1 AND token = $2 AND status IN ('planned', 'sent'))",
    )
    .bind(chain_id)
    .bind(address_hex(token))
    .fetch_one(&mut **transaction)
    .await
}

/// Locks and returns the oldest planned flush, provided no transaction is already in flight.
pub async fn next_planned_flush(
    transaction: &mut Transaction<'_, Postgres>,
    chain_id: u64,
    operator: Address,
) -> Result<Option<Flush>, sqlx::Error> {
    if has_sent_flush(transaction, chain_id, operator).await? {
        return Ok(None);
    }
    let chain_id = to_i64(chain_id, "flushes.chain_id")?;
    let operator = address_hex(operator);
    let row = sqlx::query(
        r#"
        SELECT id, chain_id, token, operator, nonce::text AS nonce, tx_hash, block_number,
               status, COALESCE(receipt, '{}'::jsonb) AS receipt
        FROM flushes
        WHERE chain_id = $1 AND operator = $2 AND status = 'planned'
        ORDER BY nonce, id
        FOR UPDATE SKIP LOCKED
        LIMIT 1
        "#,
    )
    .bind(chain_id)
    .bind(operator)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(parse_flush_row).transpose()
}

/// Voids an unsigned plan held by a flush pause and moves later plans down onto its nonce.
///
/// The caller holds the operator nonce lock and the plan's row lock. The plan's addresses are
/// planned again once the pause lifts, so a pause on one route, product, or account never holds
/// the operator nonce needed by every later flush on the chain. The audit reason keeps the token
/// and address identifiers so operators can confirm that later plans cover them.
pub async fn void_paused_plan(
    transaction: &mut Transaction<'_, Postgres>,
    plan: &Flush,
    address_ids: &[Uuid],
    paused: &str,
) -> Result<(), sqlx::Error> {
    let deleted = sqlx::query("DELETE FROM flushes WHERE id = $1 AND status = 'planned'")
        .bind(plan.id)
        .execute(&mut **transaction)
        .await?;
    require_one(deleted.rows_affected(), "planned flush was not available")?;
    let chain_id = to_i64(plan.chain_id, "flushes.chain_id")?;
    let operator = address_hex(plan.operator);
    let later = sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT id
        FROM flushes
        WHERE chain_id = $1 AND operator = $2 AND status = 'planned' AND nonce > $3::text::numeric
        ORDER BY nonce, id
        FOR UPDATE
        "#,
    )
    .bind(chain_id)
    .bind(&operator)
    .bind(plan.nonce.to_string())
    .fetch_all(&mut **transaction)
    .await?;
    let mut nonce = plan.nonce;
    for id in &later {
        sqlx::query("UPDATE flushes SET nonce = $2::text::numeric WHERE id = $1")
            .bind(id)
            .bind(nonce.to_string())
            .execute(&mut **transaction)
            .await?;
        nonce = nonce.checked_add(1).ok_or_else(|| {
            sqlx::Error::Protocol("flush nonce overflowed while voiding a plan".to_owned())
        })?;
    }
    sqlx::query(
        r#"
        INSERT INTO audit (id, actor, action, subject, reason)
        VALUES ($1, 'flusher', 'flush.send_paused', $2, $3)
        "#,
    )
    .bind(Uuid::new_v4())
    .bind(plan.id.to_string())
    .bind(format!(
        "voided unsigned plan for token {} at operator {operator} nonce {}: {paused} has the \
         flush scope paused; address ids [{}]; {} later plan(s) moved down one nonce",
        address_hex(plan.token),
        plan.nonce,
        address_ids
            .iter()
            .map(Uuid::to_string)
            .collect::<Vec<_>>()
            .join(", "),
        later.len()
    ))
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Marks a planned flush as sent before its raw transaction is broadcast.
pub async fn mark_flush_sent(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    tx_hash: B256,
    receipt: &Value,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE flushes SET status = 'sent', tx_hash = $2, receipt = $3 WHERE id = $1 AND status = 'planned'",
    )
    .bind(id)
    .bind(b256_hex(tx_hash))
    .bind(receipt)
    .execute(&mut **transaction)
    .await?;
    require_one(result.rows_affected(), "planned flush was not available")
}

/// Replaces the durable raw transaction and hash history while preserving the nonce.
pub async fn store_flush_replacement(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    tx_hash: B256,
    receipt: &Value,
) -> Result<(), sqlx::Error> {
    let result = sqlx::query(
        "UPDATE flushes SET tx_hash = $2, receipt = $3 WHERE id = $1 AND status = 'sent'",
    )
    .bind(id)
    .bind(b256_hex(tx_hash))
    .bind(receipt)
    .execute(&mut **transaction)
    .await?;
    require_one(result.rows_affected(), "sent flush was not available")
}

/// Stores a replacement only if the caller still owns the observed current hash.
pub async fn store_flush_replacement_cas(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    expected_hash: B256,
    tx_hash: B256,
    receipt: &Value,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE flushes SET tx_hash = $3, receipt = $4 WHERE id = $1 AND status = 'sent' AND tx_hash = $2",
    )
    .bind(id)
    .bind(b256_hex(expected_hash))
    .bind(b256_hex(tx_hash))
    .bind(receipt)
    .execute(&mut **transaction)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Replaces sent evidence without changing the transaction hash.
pub async fn update_sent_evidence(
    pool: &PgPool,
    id: Uuid,
    expected_hash: B256,
    receipt: &Value,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE flushes SET receipt = $3 WHERE id = $1 AND status = 'sent' AND tx_hash = $2",
    )
    .bind(id)
    .bind(b256_hex(expected_hash))
    .bind(receipt)
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}

/// Lists all flushes in one status, ordered deterministically.
pub async fn list_flushes(pool: &PgPool, status: FlushStatus) -> Result<Vec<Flush>, sqlx::Error> {
    let rows = sqlx::query(
        r#"
        SELECT id, chain_id, token, operator, nonce::text AS nonce, tx_hash, block_number,
               status, COALESCE(receipt, '{}'::jsonb) AS receipt
        FROM flushes
        WHERE status = $1
        ORDER BY chain_id, operator, nonce
        "#,
    )
    .bind(status.code())
    .fetch_all(pool)
    .await?;
    rows.into_iter().map(parse_flush_row).collect()
}

/// Fetches one flush by identifier.
pub async fn get_flush(pool: &PgPool, id: Uuid) -> Result<Option<Flush>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, chain_id, token, operator, nonce::text AS nonce, tx_hash, block_number,
               status, COALESCE(receipt, '{}'::jsonb) AS receipt
        FROM flushes WHERE id = $1
        "#,
    )
    .bind(id)
    .fetch_optional(pool)
    .await?;
    row.map(parse_flush_row).transpose()
}

/// Locks and fetches one flush by identifier inside a caller-owned transaction.
pub async fn get_flush_locked(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
) -> Result<Option<Flush>, sqlx::Error> {
    let row = sqlx::query(
        r#"
        SELECT id, chain_id, token, operator, nonce::text AS nonce, tx_hash, block_number,
               status, COALESCE(receipt, '{}'::jsonb) AS receipt
        FROM flushes WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(id)
    .fetch_optional(&mut **transaction)
    .await?;
    row.map(parse_flush_row).transpose()
}

/// Marks a flush reverted and stores its finalized receipt evidence.
pub async fn mark_flush_reverted(
    pool: &PgPool,
    id: Uuid,
    block_number: u64,
    receipt: &Value,
) -> Result<(), sqlx::Error> {
    let block_number = to_i64(block_number, "flushes.block_number")?;
    let result = sqlx::query(
        "UPDATE flushes SET status = 'reverted', block_number = $2, receipt = $3 WHERE id = $1 AND status = 'sent'",
    )
    .bind(id)
    .bind(block_number)
    .bind(receipt)
    .execute(pool)
    .await?;
    if result.rows_affected() > 1 {
        return Err(sqlx::Error::Protocol(
            "flush update affected multiple rows".to_owned(),
        ));
    }
    Ok(())
}

/// Marks a sent flush reverted inside a caller-owned transaction.
pub async fn mark_flush_reverted_locked(
    transaction: &mut Transaction<'_, Postgres>,
    id: Uuid,
    block_number: u64,
    receipt: &Value,
) -> Result<(), sqlx::Error> {
    let block_number = to_i64(block_number, "flushes.block_number")?;
    let result = sqlx::query(
        "UPDATE flushes SET status = 'reverted', block_number = $2, receipt = $3 WHERE id = $1 AND status = 'sent'",
    )
    .bind(id)
    .bind(block_number)
    .bind(receipt)
    .execute(&mut **transaction)
    .await?;
    require_one(result.rows_affected(), "sent flush was not available")
}

/// Confirms a finalized flush, persists its events, links deposits, and records swept transitions.
pub async fn confirm_flush(
    pool: &PgPool,
    id: Uuid,
    block_number: u64,
    receipt: &Value,
    events: &[FlushedEvent],
) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let block_number_db = to_i64(block_number, "flushes.block_number")?;
    let updated = sqlx::query(
        r#"
        UPDATE flushes
        SET status = 'confirmed', block_number = $2, receipt = $3
        WHERE id = $1 AND status = 'sent'
        "#,
    )
    .bind(id)
    .bind(block_number_db)
    .bind(receipt)
    .execute(&mut *transaction)
    .await?;
    if updated.rows_affected() == 0 {
        let confirmed = sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS(SELECT 1 FROM flushes WHERE id = $1 AND status = 'confirmed')",
        )
        .bind(id)
        .fetch_one(&mut *transaction)
        .await?;
        if confirmed {
            transaction.commit().await?;
            return Ok(());
        }
        return Err(sqlx::Error::Protocol(
            "flush was not sent and cannot be confirmed".to_owned(),
        ));
    }

    for event in events {
        insert_flushed_in(&mut transaction, event).await?;
    }
    link_deposits_for_flush(&mut transaction, id).await?;
    transaction.commit().await
}

/// Inserts one confirmed flush event.
pub async fn insert_flushed(pool: &PgPool, event: &FlushedEvent) -> Result<(), sqlx::Error> {
    let mut transaction = pool.begin().await?;
    insert_flushed_in(&mut transaction, event).await?;
    transaction.commit().await
}

async fn insert_flushed_in(
    transaction: &mut Transaction<'_, Postgres>,
    event: &FlushedEvent,
) -> Result<(), sqlx::Error> {
    let amount_atomic = atomic_decimal(event.amount_atomic);
    let block_number = to_i64(event.block_number, "flushed.block_number")?;
    let log_index = to_i64(event.log_index, "flushed.log_index")?;
    sqlx::query(
        r#"
        INSERT INTO flushed (flush_id, address_id, amount_atomic, block_number, log_index)
        VALUES ($1, $2, $3::text::numeric, $4, $5)
        "#,
    )
    .bind(event.flush_id)
    .bind(event.address_id)
    .bind(amount_atomic)
    .bind(block_number)
    .bind(log_index)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

/// Replays the §7 linkage rule for one confirmed flush and returns the deposits it linked.
///
/// The target state is computed from the row locked by this statement, so a deposit advanced
/// concurrently is never written back to an older state.
pub async fn link_confirmed_flush(pool: &PgPool, flush_id: Uuid) -> Result<Vec<Uuid>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let linked = link_deposits_for_flush(&mut transaction, flush_id).await?;
    transaction.commit().await?;
    Ok(linked)
}

async fn link_deposits_for_flush(
    transaction: &mut Transaction<'_, Postgres>,
    flush_id: Uuid,
) -> Result<Vec<Uuid>, sqlx::Error> {
    let evidence = json!({"flush_id": flush_id, "outcome": "advance"});
    sqlx::query_scalar::<_, Uuid>(
        r#"
        WITH eligible AS (
            SELECT d.id, d.state
            FROM deposits d
            JOIN flushed f ON f.address_id = d.address_id
            JOIN flushes x ON x.id = f.flush_id
            WHERE f.flush_id = $1
              AND x.status = 'confirmed'
              AND d.asset_contract = x.token
              AND d.flush_id IS NULL
              AND (d.block_number, d.log_index) < (f.block_number, f.log_index)
            FOR UPDATE OF d
        ), updated AS (
            UPDATE deposits d
            SET flush_id = $1,
                state = CASE WHEN d.state = 'credited' THEN 'swept' ELSE d.state END,
                attempt = CASE WHEN d.state = 'credited' THEN 0 ELSE d.attempt END,
                lease_token = CASE WHEN d.state = 'credited' THEN NULL ELSE d.lease_token END,
                lease_until = CASE WHEN d.state = 'credited' THEN NULL ELSE d.lease_until END,
                updated_at = now()
            FROM eligible e
            WHERE d.id = e.id
            RETURNING d.id, e.state
        ), swept AS (
            INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
            SELECT gen_random_uuid(), id, 'credited', 'swept', 0, $2
            FROM updated
            WHERE state = 'credited'
        )
        SELECT id FROM updated ORDER BY id
        "#,
    )
    .bind(flush_id)
    .bind(evidence)
    .fetch_all(&mut **transaction)
    .await
}

fn parse_flush_row(row: sqlx::postgres::PgRow) -> Result<Flush, sqlx::Error> {
    let chain_id: i64 = row.try_get("chain_id")?;
    let token: String = row.try_get("token")?;
    let operator: String = row.try_get("operator")?;
    let nonce: String = row.try_get("nonce")?;
    let tx_hash: Option<String> = row.try_get("tx_hash")?;
    let block_number: Option<i64> = row.try_get("block_number")?;
    let status: String = row.try_get("status")?;
    Ok(Flush {
        id: row.try_get("id")?,
        chain_id: to_u64(chain_id, "flushes.chain_id")?,
        token: parse_address(&token)?,
        operator: parse_address(&operator)?,
        nonce: parse_u64_decimal(&nonce, "flushes.nonce")?,
        tx_hash: tx_hash.as_deref().map(parse_b256).transpose()?,
        block_number: block_number
            .map(|value| to_u64(value, "flushes.block_number"))
            .transpose()?,
        status: FlushStatus::parse(&status)?,
        receipt: row.try_get("receipt")?,
    })
}

fn parse_u64_decimal(value: &str, field: &'static str) -> Result<u64, sqlx::Error> {
    value
        .parse::<u64>()
        .map_err(|error| sqlx::Error::Decode(format!("{field} is outside u64: {error}").into()))
}

fn require_one(rows: u64, message: &'static str) -> Result<(), sqlx::Error> {
    if rows == 1 {
        Ok(())
    } else {
        Err(sqlx::Error::Protocol(message.to_owned()))
    }
}
