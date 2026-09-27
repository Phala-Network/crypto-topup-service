use chrono::{DateTime, Utc};
use sqlx::PgExecutor;
use uuid::Uuid;

/// The object an event is about; the event's `data.object` is its API representation, rendered
/// on the first delivery attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventObject {
    /// A deposit, by its UUID.
    Deposit(Uuid),
    /// A quote, by its address row id.
    Quote(Uuid),
}

impl EventObject {
    /// The stored object type.
    #[must_use]
    pub const fn type_code(self) -> &'static str {
        match self {
            Self::Deposit(_) => "deposit",
            Self::Quote(_) => "quote",
        }
    }

    /// Reads a stored object type and id.
    #[must_use]
    pub fn from_parts(type_code: &str, id: Uuid) -> Option<Self> {
        match type_code {
            "deposit" => Some(Self::Deposit(id)),
            "quote" => Some(Self::Quote(id)),
            _ => None,
        }
    }

    /// The object's UUID.
    #[must_use]
    pub const fn id(self) -> Uuid {
        match self {
            Self::Deposit(id) | Self::Quote(id) => id,
        }
    }
}

/// Values used to enqueue an event.
#[derive(Clone, Debug, PartialEq)]
pub struct NewOutboxEvent {
    /// Event identifier, derived from the event type and its subject (`identity::event_id`).
    pub id: Uuid,
    /// Stable event type.
    pub event_type: String,
    /// Product the event is delivered to.
    pub product_id: Uuid,
    /// Object the event is about.
    pub object: EventObject,
    /// Earliest delivery attempt.
    pub next_attempt_at: DateTime<Utc>,
}

/// Enqueues an event for at-least-once delivery; an event already enqueued is kept unchanged.
pub async fn enqueue_in<'e>(
    executor: impl PgExecutor<'e>,
    event: &NewOutboxEvent,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO outbox (id, event_type, payload, next_attempt_at, format, product_id,
                            object_type, object_id)
        VALUES ($1, $2, '{}', $3, 2, $4, $5, $6)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(event.id)
    .bind(&event.event_type)
    .bind(event.next_attempt_at)
    .bind(event.product_id)
    .bind(event.object.type_code())
    .bind(event.object.id())
    .execute(executor)
    .await?;
    Ok(())
}
