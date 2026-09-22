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

/// A pending event reserved for delivery for five minutes.
#[derive(Clone, Debug, PartialEq)]
pub struct ClaimedOutboxEvent {
    /// Event identifier.
    pub id: Uuid,
    /// Stable event type.
    pub event_type: String,
    /// Event payload.
    pub payload: Value,
    /// Reservation expiry represented by the next eligible attempt time.
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

/// Claims one due event by moving its next eligible attempt five minutes forward.
pub async fn claim_outbox(pool: &PgPool) -> Result<Option<ClaimedOutboxEvent>, sqlx::Error> {
    sqlx::query_as!(
        ClaimedOutboxEvent,
        r#"
        WITH candidate AS (
            SELECT id
            FROM outbox
            WHERE delivered_at IS NULL AND next_attempt_at <= now()
            ORDER BY next_attempt_at, id
            FOR UPDATE SKIP LOCKED
            LIMIT 1
        )
        UPDATE outbox AS event
        SET next_attempt_at = now() + interval '5 minutes'
        FROM candidate
        WHERE event.id = candidate.id
        RETURNING event.id, event.event_type, event.payload, event.next_attempt_at
        "#
    )
    .fetch_optional(pool)
    .await
}

/// Marks a pending event delivered and stores the receiver response.
pub async fn mark_delivered(
    pool: &PgPool,
    id: Uuid,
    response: &Value,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query!(
        r#"
        UPDATE outbox
        SET delivered_at = now(), response = $2
        WHERE id = $1 AND delivered_at IS NULL
        "#,
        id,
        response
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected() == 1)
}
