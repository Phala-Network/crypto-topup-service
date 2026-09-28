use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::audit::RequestRef;

/// Returns an event's `webhook-id`, its `evt_` id.
#[must_use]
pub fn webhook_id(id: Uuid) -> String {
    crate::ids::format(crate::ids::EVENT, id)
}

/// Event body delivered to webhook endpoints, Stripe's Event object.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Event {
    /// `evt_` id, also sent as `webhook-id`.
    pub id: String,
    /// Always `event`.
    pub object: &'static str,
    /// The account the event belongs to, `acct_…`. Receivers check it against their own account:
    /// an event for another account never verifies with their key, and is refused if it does.
    pub account: String,
    /// Whether the event happened in live mode; each mode has its own key.
    pub livemode: bool,
    /// Stable, full-stop-delimited event type.
    #[serde(rename = "type")]
    pub event_type: String,
    /// Unix seconds when the event was created, unchanged across retries.
    pub created: i64,
    /// Who caused the event: an API key id (`key_…`), `admin`, or `system`.
    pub actor: String,
    /// The API request that caused the event, Stripe's `request`: its `Request-Id` and
    /// `Idempotency-Key`; `null` for the service's own workers.
    pub request: Option<RequestRef>,
    /// `{"object": …}`, with `previous_attributes` on `*.updated` events: the object's API
    /// representation when the event was created, never re-rendered.
    pub data: Value,
}
