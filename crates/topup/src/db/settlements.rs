use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{PgPool, Postgres, Transaction};
use uuid::Uuid;

/// Persisted settlement lifecycle status.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SettlementStatus {
    /// The immutable request is durable but has not been sent.
    Intent,
    /// A request was sent without a known product answer.
    Sent,
    /// The product accepted the settlement.
    Accepted,
    /// The product rejected the settlement.
    Rejected,
}

impl SettlementStatus {
    fn parse(value: &str) -> Result<Self, sqlx::Error> {
        match value {
            "intent" => Ok(Self::Intent),
            "sent" => Ok(Self::Sent),
            "accepted" => Ok(Self::Accepted),
            "rejected" => Ok(Self::Rejected),
            other => Err(sqlx::Error::Decode(
                format!("unknown settlement status `{other}`").into(),
            )),
        }
    }
}

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
    /// Settlement lifecycle status.
    pub status: SettlementStatus,
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

#[derive(Debug, sqlx::FromRow)]
struct SettlementRecord {
    deposit_id: Uuid,
    product_id: Uuid,
    key: String,
    payload: Value,
    status: String,
    destination_tx_id: Option<String>,
    receipt: Option<Value>,
    sent_at: Option<DateTime<Utc>>,
}

impl TryFrom<SettlementRecord> for Settlement {
    type Error = sqlx::Error;

    fn try_from(record: SettlementRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            deposit_id: record.deposit_id,
            product_id: record.product_id,
            key: record.key,
            payload: record.payload,
            status: SettlementStatus::parse(&record.status)?,
            destination_tx_id: record.destination_tx_id,
            receipt: record.receipt,
            sent_at: record.sent_at,
        })
    }
}

/// Inserts a settlement intent or returns the original intent for that deposit.
pub async fn upsert_intent(
    pool: &PgPool,
    intent: &SettlementIntent,
) -> Result<Settlement, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let settlement = upsert_intent_in(&mut transaction, intent).await?;
    transaction.commit().await?;
    Ok(settlement)
}

/// Inserts a settlement intent inside an existing transaction.
pub async fn upsert_intent_in(
    transaction: &mut Transaction<'_, Postgres>,
    intent: &SettlementIntent,
) -> Result<Settlement, sqlx::Error> {
    let record = sqlx::query_as!(
        SettlementRecord,
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
    .fetch_one(&mut **transaction)
    .await?;
    record.try_into()
}

/// Stores that a request was sent without a known answer.
pub async fn mark_sent(pool: &PgPool, deposit_id: Uuid) -> Result<Settlement, sqlx::Error> {
    update_outcome(pool, deposit_id, "sent", None, None, true).await
}

/// Stores that a request was sent with durable recovery evidence.
pub async fn mark_sent_with_receipt(
    pool: &PgPool,
    deposit_id: Uuid,
    receipt: &Value,
) -> Result<Settlement, sqlx::Error> {
    update_outcome(pool, deposit_id, "sent", None, Some(receipt), true).await
}

/// Stores the product's accepted answer.
pub async fn mark_accepted(
    pool: &PgPool,
    deposit_id: Uuid,
    destination_tx_id: &str,
    receipt: &Value,
) -> Result<Settlement, sqlx::Error> {
    update_outcome(
        pool,
        deposit_id,
        "accepted",
        Some(destination_tx_id),
        Some(receipt),
        true,
    )
    .await
}

/// Stores the product's rejected answer.
pub async fn mark_rejected(
    pool: &PgPool,
    deposit_id: Uuid,
    receipt: &Value,
) -> Result<Settlement, sqlx::Error> {
    update_outcome(pool, deposit_id, "rejected", None, Some(receipt), true).await
}

async fn update_outcome(
    pool: &PgPool,
    deposit_id: Uuid,
    status: &str,
    destination_tx_id: Option<&str>,
    receipt: Option<&Value>,
    sent: bool,
) -> Result<Settlement, sqlx::Error> {
    let record = sqlx::query_as::<_, SettlementRecord>(
        r#"
        UPDATE settlements
        SET status = $2,
            destination_tx_id = $3,
            receipt = COALESCE($4, receipt),
            sent_at = CASE WHEN $5 THEN now() ELSE sent_at END
        WHERE deposit_id = $1
        RETURNING deposit_id, product_id, key, payload, status, destination_tx_id, receipt, sent_at
        "#,
    )
    .bind(deposit_id)
    .bind(status)
    .bind(destination_tx_id)
    .bind(receipt)
    .bind(sent)
    .fetch_one(pool)
    .await?;
    record.try_into()
}
