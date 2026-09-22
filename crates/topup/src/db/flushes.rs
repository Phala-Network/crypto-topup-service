use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Values used to persist a planned or observed flush transaction.
#[derive(Clone, Debug, PartialEq)]
pub struct NewFlush {
    /// Flush identifier.
    pub id: Uuid,
    /// EVM chain identifier.
    pub chain_id: i64,
    /// Token contract address.
    pub token: String,
    /// Operator address.
    pub operator: String,
    /// Operator transaction nonce as an unsigned decimal string.
    pub nonce: String,
    /// Transaction hash when sent.
    pub tx_hash: Option<String>,
    /// Finalized block number when confirmed.
    pub block_number: Option<i64>,
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
    /// Atomic token amount as an unsigned decimal string.
    pub amount_atomic: String,
    /// Event block number.
    pub block_number: i64,
    /// Event log index.
    pub log_index: i64,
}

/// Inserts a flush transaction row.
pub async fn insert_flush(pool: &PgPool, flush: &NewFlush) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO flushes
            (id, chain_id, token, operator, nonce, tx_hash, block_number, status, receipt)
        VALUES ($1, $2, $3, $4, $5::text::numeric, $6, $7, $8, $9)
        "#,
        flush.id,
        flush.chain_id,
        flush.token,
        flush.operator,
        flush.nonce,
        flush.tx_hash,
        flush.block_number,
        flush.status,
        flush.receipt
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Inserts one confirmed flush event.
pub async fn insert_flushed(pool: &PgPool, event: &FlushedEvent) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        INSERT INTO flushed (flush_id, address_id, amount_atomic, block_number, log_index)
        VALUES ($1, $2, $3::text::numeric, $4, $5)
        "#,
        event.flush_id,
        event.address_id,
        event.amount_atomic,
        event.block_number,
        event.log_index
    )
    .execute(pool)
    .await?;
    Ok(())
}
