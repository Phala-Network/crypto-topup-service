use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Outbox format of rows written before Stripe-style events: a flat payload in [`EventEnvelope`].
pub const LEGACY_FORMAT: i16 = 1;

/// Returns an event's `webhook-id`: its `evt_` id, or the bare UUID for a format-1 event.
#[must_use]
pub fn webhook_id(format: i16, id: Uuid) -> String {
    if format == LEGACY_FORMAT {
        id.to_string()
    } else {
        crate::ids::format(crate::ids::EVENT, id)
    }
}

/// Event body delivered to product webhook endpoints, Stripe's Event object.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    /// `evt_` id, also sent as `webhook-id`.
    pub id: String,
    /// Always `event`.
    pub object: &'static str,
    /// Stable, full-stop-delimited event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Unix seconds when the event was created, unchanged across retries.
    pub created: i64,
    /// `{"object": …}`: the object's API representation, never re-rendered.
    pub data: Value,
}

/// Event body of rows written before Stripe-style events (outbox format 1), kept so a replay of
/// an old event is byte-identical to its first delivery.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EventEnvelope {
    /// Stable outbox identifier, also sent as `webhook-id`.
    pub event_id: Uuid,
    /// Stable, full-stop-delimited event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Time the outbox row was created, unchanged across retries.
    pub created_at: DateTime<Utc>,
    /// Event-specific payload.
    pub data: Value,
}
