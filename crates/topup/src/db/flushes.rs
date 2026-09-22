use alloy_primitives::{Address, B256};
use serde_json::Value;
use sqlx::PgPool;
use topup_core::money::AtomicAmount;
use uuid::Uuid;

use super::types::{address_hex, atomic_decimal, b256_hex, to_i64};

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
    sqlx::query!(
        r#"
        INSERT INTO flushes
            (id, chain_id, token, operator, nonce, tx_hash, block_number, status, receipt)
        VALUES ($1, $2, $3, $4, $5::text::numeric, $6, $7, $8, $9)
        "#,
        flush.id,
        chain_id,
        token,
        operator,
        nonce,
        tx_hash,
        block_number,
        flush.status,
        flush.receipt
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Inserts one confirmed flush event.
pub async fn insert_flushed(pool: &PgPool, event: &FlushedEvent) -> Result<(), sqlx::Error> {
    let amount_atomic = atomic_decimal(event.amount_atomic);
    let block_number = to_i64(event.block_number, "flushed.block_number")?;
    let log_index = to_i64(event.log_index, "flushed.log_index")?;
    sqlx::query!(
        r#"
        INSERT INTO flushed (flush_id, address_id, amount_atomic, block_number, log_index)
        VALUES ($1, $2, $3::text::numeric, $4, $5)
        "#,
        event.flush_id,
        event.address_id,
        amount_atomic,
        block_number,
        log_index
    )
    .execute(pool)
    .await?;
    Ok(())
}
