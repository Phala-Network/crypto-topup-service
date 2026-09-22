use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// A durable product settlement record.
#[derive(Clone, Debug, PartialEq)]
pub struct Settlement {
    /// Deposit being settled.
    pub deposit_id: Uuid,
    /// Destination product.
    pub product_id: Uuid,
    /// Product idempotency key.
    pub key: String,
    /// Original settlement payload.
    pub payload: Value,
    /// Settlement status code.
    pub status: String,
    /// Product transaction identifier after acceptance.
    pub destination_tx_id: Option<String>,
    /// Stored product response.
    pub receipt: Option<Value>,
    /// Most recent send time.
    pub sent_at: Option<DateTime<Utc>>,
}

/// Values used to persist a settlement intent before sending.
#[derive(Clone, Debug, PartialEq)]
pub struct SettlementIntent {
    /// Deposit being settled.
    pub deposit_id: Uuid,
    /// Destination product.
    pub product_id: Uuid,
    /// Product idempotency key.
    pub key: String,
    /// Immutable original settlement payload.
    pub payload: Value,
}

/// Inserts a settlement intent or returns the original intent for that deposit.
pub async fn upsert_intent(
    pool: &PgPool,
    intent: &SettlementIntent,
) -> Result<Settlement, sqlx::Error> {
    sqlx::query_as!(
        Settlement,
        r#"
        INSERT INTO settlements (deposit_id, product_id, key, payload, status)
        VALUES ($1, $2, $3, $4, 'intent')
        ON CONFLICT (deposit_id) DO UPDATE SET deposit_id = EXCLUDED.deposit_id
        RETURNING deposit_id, product_id, key, payload, status, destination_tx_id, receipt, sent_at
        "#,
        intent.deposit_id,
        intent.product_id,
        intent.key,
        intent.payload
    )
    .fetch_one(pool)
    .await
}
