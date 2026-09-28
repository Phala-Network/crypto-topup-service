use chrono::{DateTime, Utc};
use serde_json::Value;
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
    /// A refund, by its UUID.
    Refund(Uuid),
    /// A treasury, by its id.
    Treasury(Uuid),
    /// A webhook endpoint, by its id.
    WebhookEndpoint(Uuid),
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
            Self::Refund(_) => "refund",
            Self::Treasury(_) => "treasury",
            Self::WebhookEndpoint(_) => "webhook_endpoint",
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
            "refund" => Some(Self::Refund(id)),
            "treasury" => Some(Self::Treasury(id)),
            "webhook_endpoint" => Some(Self::WebhookEndpoint(id)),
            _ => None,
        }
    }

    /// The object's UUID.
    #[must_use]
    pub const fn id(self) -> Uuid {
        match self {
            Self::Deposit(id)
            | Self::Quote(id)
            | Self::ApiKey(id)
            | Self::Account(id)
            | Self::Refund(id)
            | Self::Treasury(id)
            | Self::WebhookEndpoint(id) => id,
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

/// Whether `event_type` is an account event: a change to the account, its keys, or its webhook
/// endpoints (and, with design PR 7, its treasuries, `account.treasury.*`). Account events reach
/// every enabled endpoint of the account and mode, whatever its `enabled_events` (design §11), so a
/// merchant cannot miss one by filtering; GitHub's `meta` event is the precedent.
#[must_use]
pub fn is_account_event(event_type: &str) -> bool {
    ["account.", "api_key.", "webhook_endpoint."]
        .iter()
        .any(|prefix| event_type.starts_with(prefix))
}

/// Records an event and one delivery to each enabled webhook endpoint of its account and mode
/// that subscribes to its type, or to every one for an account event ([`is_account_event`]), for
/// at-least-once delivery (the outbox, architecture §11). An event already recorded is kept
/// unchanged, and so are its deliveries.
pub async fn enqueue_in(
    connection: &mut PgConnection,
    event: &NewOutboxEvent,
) -> Result<(), sqlx::Error> {
    if record(connection, event, None).await? {
        fan_out(connection, event).await?;
    }
    Ok(())
}

/// An endpoint's own notice: the delivery of an event about the endpoint to the URL it had before
/// the change, whatever its status now.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notice {
    /// The endpoint the event is about.
    pub endpoint_id: Uuid,
    /// Where the endpoint listened before the change.
    pub url: String,
}

/// Records an event whose `data` is already rendered, a snapshot of its object when it happened.
/// The `notice` delivery is recorded first and is delivered before the endpoint's other due
/// deliveries; with `fan_out`, the event then reaches every other endpoint as [`enqueue_in`]'s do.
pub async fn enqueue_rendered_in(
    connection: &mut PgConnection,
    event: &NewOutboxEvent,
    data: &Value,
    notice: Option<&Notice>,
    fan_out_to_endpoints: bool,
) -> Result<(), sqlx::Error> {
    if !record(connection, event, Some(data)).await? {
        return Ok(());
    }
    if let Some(notice) = notice {
        sqlx::query(
            r#"
            INSERT INTO webhook_deliveries (event_id, endpoint_id, next_attempt_at, url)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(event.id)
        .bind(notice.endpoint_id)
        .bind(event.next_attempt_at)
        .bind(&notice.url)
        .execute(&mut *connection)
        .await?;
    }
    if fan_out_to_endpoints {
        fan_out(connection, event).await?;
    }
    Ok(())
}

/// Inserts the event; `false` when it was already recorded.
async fn record(
    connection: &mut PgConnection,
    event: &NewOutboxEvent,
    data: Option<&Value>,
) -> Result<bool, sqlx::Error> {
    let inserted = sqlx::query(
        r#"
        INSERT INTO events (id, account_id, livemode, type, object_type, object_id, actor, data)
        VALUES ($1, $2, $3, $4, $5, $6, $7, COALESCE($8, '{}'::jsonb))
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
    .bind(data)
    .execute(&mut *connection)
    .await?
    .rows_affected();
    Ok(inserted > 0)
}

async fn fan_out(connection: &mut PgConnection, event: &NewOutboxEvent) -> Result<(), sqlx::Error> {
    sqlx::query(
        r#"
        INSERT INTO webhook_deliveries (event_id, endpoint_id, next_attempt_at)
        SELECT $1, endpoint.id, $5
        FROM webhook_endpoints AS endpoint
        WHERE endpoint.account_id = $2
          AND endpoint.livemode = $3
          AND endpoint.status = 'enabled'
          AND endpoint.deleted_at IS NULL
          AND ($6 OR '*' = ANY(endpoint.enabled_events) OR $4 = ANY(endpoint.enabled_events))
        ON CONFLICT (event_id, endpoint_id) DO NOTHING
        "#,
    )
    .bind(event.id)
    .bind(event.account_id)
    .bind(event.livemode)
    .bind(&event.event_type)
    .bind(event.next_attempt_at)
    .bind(is_account_event(&event.event_type))
    .execute(&mut *connection)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::is_account_event;

    #[test]
    fn account_events_are_the_account_key_and_endpoint_changes() {
        for event_type in [
            "account.updated",
            "account.treasury.pending",
            "api_key.created",
            "api_key.revoked",
            "webhook_endpoint.updated",
            "webhook_endpoint.deleted",
        ] {
            assert!(is_account_event(event_type), "{event_type}");
        }
        for event_type in [
            "deposit.credited",
            "quote.expired",
            "refund.failed",
            "accounts",
        ] {
            assert!(!is_account_event(event_type), "{event_type}");
        }
    }
}
