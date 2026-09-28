use serde::Serialize;
use serde_json::Value;
use sqlx::PgPool;
use uuid::Uuid;

use crate::db::EventObject;
use crate::routes::RouteSet;
use crate::tenancy::Scope;

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
    /// `{"object": …}`: the object's API representation, never re-rendered.
    pub data: Value,
}

/// An event's `data`: `stored` when it holds the object, else the object rendered now and stored
/// with the event, so every endpoint, retry, resend, and read gets the same `data`. Another
/// renderer's stored value wins a race. `Ok(Err(code))` names why it cannot be rendered.
pub(crate) async fn event_data(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    event_id: Uuid,
    object: Option<EventObject>,
    stored: &Value,
) -> Result<Result<Value, &'static str>, sqlx::Error> {
    if stored.get("object").is_some() {
        return Ok(Ok(stored.clone()));
    }
    let Some(object) = object else {
        return Ok(Err("missing_object"));
    };
    let data = match crate::api::event_data(pool, routes, scope, object).await {
        Ok(Some(data)) => data,
        Ok(None) => return Ok(Err("object_not_found")),
        Err(()) => return Ok(Err("render_failed")),
    };
    let stored = sqlx::query_scalar::<_, Value>(
        r#"
        UPDATE events
        SET data = CASE WHEN data = '{}'::jsonb THEN $2 ELSE data END
        WHERE id = $1
        RETURNING data
        "#,
    )
    .bind(event_id)
    .bind(&data)
    .fetch_one(pool)
    .await?;
    Ok(Ok(stored))
}
