use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

/// The object an event is about; the event's `data.object` is its API representation, rendered
/// on the first delivery attempt.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventObject {
    /// A deposit, by its UUID.
    Deposit(Uuid),
    /// A quote, by its id.
    Quote(Uuid),
    /// An API key, by its id.
    ApiKey(Uuid),
    /// The account itself.
    Account(Uuid),
}

impl EventObject {
    /// The stored object type.
    #[must_use]
    pub const fn type_code(self) -> &'static str {
        match self {
            Self::Deposit(_) => "deposit",
            Self::Quote(_) => "quote",
            Self::ApiKey(_) => "api_key",
            Self::Account(_) => "account",
        }
    }

    /// Reads a stored object type and id.
    #[must_use]
    pub fn from_parts(type_code: &str, id: Uuid) -> Option<Self> {
        match type_code {
            "deposit" => Some(Self::Deposit(id)),
            "quote" => Some(Self::Quote(id)),
            "api_key" => Some(Self::ApiKey(id)),
            "account" => Some(Self::Account(id)),
            _ => None,
        }
    }

    /// The object's UUID.
    #[must_use]
    pub const fn id(self) -> Uuid {
        match self {
            Self::Deposit(id) | Self::Quote(id) | Self::ApiKey(id) | Self::Account(id) => id,
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
    /// Account the event belongs to: the object's account.
    pub account_id: Uuid,
    /// Mode of the object.
    pub livemode: bool,
    /// Object the event is about.
    pub object: EventObject,
    /// Earliest delivery attempt.
    pub next_attempt_at: DateTime<Utc>,
    /// Who caused the event: an API key id (`key_…`), `admin`, or [`SYSTEM_ACTOR`].
    pub actor: String,
}

/// The actor of events the service's own workers cause.
pub const SYSTEM_ACTOR: &str = "system";

/// Records an event and one delivery to each enabled webhook endpoint of its account and mode
/// that subscribes to its type, for at-least-once delivery (the outbox, architecture §11). An
/// event already recorded is kept unchanged, and so are its deliveries.
pub async fn enqueue_in(
    connection: &mut PgConnection,
    event: &NewOutboxEvent,
) -> Result<(), sqlx::Error> {
    let inserted = sqlx::query(
        r#"
        INSERT INTO events (id, account_id, livemode, type, object_type, object_id, actor)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(event.id)
    .bind(event.account_id)
    .bind(event.livemode)
    .bind(&event.event_type)
    .bind(event.object.type_code())
    .bind(event.object.id())
    .bind(&event.actor)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Ok(());
    }
    sqlx::query(
        r#"
        INSERT INTO webhook_deliveries (event_id, endpoint_id, next_attempt_at)
        SELECT $1, endpoint.id, $5
        FROM webhook_endpoints AS endpoint
        WHERE endpoint.account_id = $2
          AND endpoint.livemode = $3
          AND endpoint.status = 'enabled'
          AND ('*' = ANY(endpoint.enabled_events) OR $4 = ANY(endpoint.enabled_events))
        "#,
    )
    .bind(event.id)
    .bind(event.account_id)
    .bind(event.livemode)
    .bind(&event.event_type)
    .bind(event.next_attempt_at)
    .execute(&mut *connection)
    .await?;
    Ok(())
}
