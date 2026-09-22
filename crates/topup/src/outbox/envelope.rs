use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

/// Stable event body delivered to product webhook endpoints.
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
