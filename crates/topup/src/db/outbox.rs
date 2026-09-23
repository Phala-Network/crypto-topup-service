use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

/// Values used to enqueue an event.
#[derive(Clone, Debug, PartialEq)]
pub struct NewOutboxEvent {
    /// Event identifier.
    pub id: Uuid,
    /// Stable event type.
    pub event_type: String,
    /// Event payload.
    pub payload: Value,
    /// Earliest delivery attempt.
    pub next_attempt_at: DateTime<Utc>,
}

/// Enqueues an event for at-least-once delivery.
pub async fn enqueue(pool: &PgPool, event: &NewOutboxEvent) -> Result<(), sqlx::Error> {
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
    .execute(pool)
    .await?;
    Ok(())
}
