use chrono::{DateTime, Utc};
use serde_json::{Map, Value};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::audit::{Actor, RequestRef};
use crate::routes::RouteSet;
use crate::tenancy::Scope;

/// The object an event is about; the event's `data.object` is its API representation, rendered
/// when the event is recorded.
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

    /// The object's public id, as the API and the logs show it (`dep_…`, `qt_…`, …).
    #[must_use]
    pub fn public_id(self) -> String {
        let prefix = match self {
            Self::Deposit(_) => crate::ids::DEPOSIT,
            Self::Quote(_) => crate::ids::QUOTE,
            Self::ApiKey(_) => crate::ids::API_KEY,
            Self::Account(_) => crate::ids::ACCOUNT,
            Self::Refund(_) => crate::ids::REFUND,
            Self::Treasury(_) => crate::ids::TREASURY,
            Self::WebhookEndpoint(_) => crate::ids::WEBHOOK_ENDPOINT,
        };
        crate::ids::format(prefix, self.id())
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
    /// The API request that caused the event, Stripe's event `request`; `None` for the service's
    /// own workers.
    pub request: Option<RequestRef>,
    /// A webhook key version that signs every delivery of the event beside the keys signing at
    /// delivery time: the version a webhook key roll retires, on the roll's notice.
    pub signing_key_version: Option<u32>,
}

impl NewOutboxEvent {
    /// An event of `event_type` about `object` of `scope`, caused now by `actor` (and its
    /// request), with a fresh id.
    #[must_use]
    pub fn new(event_type: &str, scope: Scope, object: EventObject, actor: &Actor) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_type: event_type.to_owned(),
            account_id: scope.account_id(),
            livemode: scope.livemode(),
            object,
            next_attempt_at: Utc::now(),
            actor: crate::api_keys::event_actor(actor),
            request: actor.request.clone(),
            signing_key_version: None,
        }
    }

    /// An event the service's own workers cause, with the id `id`.
    #[must_use]
    pub fn system(id: Uuid, event_type: &str, scope: Scope, object: EventObject) -> Self {
        Self {
            id,
            event_type: event_type.to_owned(),
            account_id: scope.account_id(),
            livemode: scope.livemode(),
            object,
            next_attempt_at: Utc::now(),
            actor: SYSTEM_ACTOR.to_owned(),
            request: None,
            signing_key_version: None,
        }
    }

    /// The account and mode the event belongs to.
    #[must_use]
    pub const fn scope(&self) -> Scope {
        Scope::new(self.account_id, self.livemode)
    }
}

/// The actor of events the service's own workers cause.
pub const SYSTEM_ACTOR: &str = "system";

/// Whether `event_type` is an account event: a change to the account, its keys, its webhook
/// endpoints, or its treasuries. Account events reach every enabled endpoint of the account and
/// mode, whatever its `enabled_events` (design §11), so a merchant cannot miss one by filtering;
/// GitHub's `meta` event is the precedent.
#[must_use]
pub fn is_account_event(event_type: &str) -> bool {
    ["account.", "api_key.", "treasury.", "webhook_endpoint."]
        .iter()
        .any(|prefix| event_type.starts_with(prefix))
}

/// The API representation of `object` as `scope` sees it now, read in the caller's transaction:
/// what an event's `data.object` holds, and, taken before a change, what `previous_attributes`
/// is computed from. An object that does not exist or cannot be rendered fails the transaction,
/// so no change is committed without its event.
pub async fn render(
    connection: &mut PgConnection,
    routes: &RouteSet,
    scope: Scope,
    object: EventObject,
) -> Result<Value, sqlx::Error> {
    match crate::api::render_object(connection, routes, scope, object).await {
        Ok(Some(value)) => Ok(value),
        Ok(None) => Err(sqlx::Error::Protocol(format!(
            "the {} of an event does not exist in its scope",
            object.type_code()
        ))),
        Err(()) => Err(sqlx::Error::Protocol(format!(
            "the {} of an event could not be rendered",
            object.type_code()
        ))),
    }
}

/// Records `event` with its object rendered now, a snapshot taken in the transaction that
/// changes it (Stripe: "the event's data is rendered at the time of the event and doesn't
/// change", <https://docs.stripe.com/api/events/object>), and one delivery to each enabled
/// endpoint of its account and mode that subscribes to its type, or to every one for an account
/// event ([`is_account_event`]). `before` is the object's representation before the change, given
/// for a `*.updated` event: `data.previous_attributes` holds what changed. An event already
/// recorded is kept unchanged, and so are its deliveries.
pub async fn enqueue_in(
    connection: &mut PgConnection,
    routes: &RouteSet,
    event: &NewOutboxEvent,
    before: Option<&Value>,
) -> Result<(), sqlx::Error> {
    // A deterministic id already recorded is a re-emission, kept unchanged: nothing to render.
    let recorded: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM events WHERE id = $1)")
        .bind(event.id)
        .fetch_one(&mut *connection)
        .await?;
    if recorded {
        return Ok(());
    }
    let object = render(connection, routes, event.scope(), event.object).await?;
    enqueue_rendered_in(connection, event, &event_data(object, before), None, true).await
}

/// `value` as JSON, an object representation for an event.
pub fn to_object<T: serde::Serialize>(value: &T) -> Result<Value, sqlx::Error> {
    serde_json::to_value(value)
        .map_err(|error| sqlx::Error::Protocol(format!("event object serialization: {error}")))
}

/// An event's `data`: `{"object": …}`, with `previous_attributes` when `before` is given.
#[must_use]
pub fn event_data(object: Value, before: Option<&Value>) -> Value {
    let mut data = Map::new();
    if let Some(before) = before {
        data.insert(
            "previous_attributes".to_owned(),
            previous_attributes(before, &object),
        );
    }
    data.insert("object".to_owned(), object);
    Value::Object(data)
}

/// Stripe's `previous_attributes`: the fields of `before` that `after` changed, with their values
/// before the change. A changed object field, such as `metadata`, holds only its changed keys; a
/// field `after` added is `null`; an array or scalar holds its whole former value.
#[must_use]
pub fn previous_attributes(before: &Value, after: &Value) -> Value {
    let empty = Map::new();
    let before_fields = before.as_object().unwrap_or(&empty);
    let after_fields = after.as_object().unwrap_or(&empty);
    let mut changed = Map::new();
    for (key, old) in before_fields {
        match after_fields.get(key) {
            Some(new) if new == old => {}
            Some(new @ Value::Object(_)) if old.is_object() => {
                changed.insert(key.clone(), previous_attributes(old, new));
            }
            _ => {
                changed.insert(key.clone(), old.clone());
            }
        }
    }
    for key in after_fields.keys() {
        if !before_fields.contains_key(key) {
            changed.insert(key.clone(), Value::Null);
        }
    }
    Value::Object(changed)
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

/// Records an event whose `data` is already rendered, a snapshot of its object when it happened;
/// every event is written through here. The `notice` delivery is recorded first and is delivered
/// before the endpoint's other due deliveries; with `fan_out_to_endpoints`, the event then
/// reaches every other endpoint as [`enqueue_in`]'s do.
pub async fn enqueue_rendered_in(
    connection: &mut PgConnection,
    event: &NewOutboxEvent,
    data: &Value,
    notice: Option<&Notice>,
    fan_out_to_endpoints: bool,
) -> Result<(), sqlx::Error> {
    if !record(connection, event, data).await? {
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
    data: &Value,
) -> Result<bool, sqlx::Error> {
    let inserted = sqlx::query(
        r#"
        INSERT INTO events (
            id, account_id, livemode, type, object_type, object_id, actor, data, request_id,
            idempotency_key, signing_key_version
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11)
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
    .bind(event.request.as_ref().map(|request| &request.id))
    .bind(
        event
            .request
            .as_ref()
            .and_then(|request| request.idempotency_key.as_ref()),
    )
    .bind(
        event
            .signing_key_version
            .map(i64::from)
            .and_then(|version| i32::try_from(version).ok()),
    )
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
    use serde_json::json;

    use super::{is_account_event, previous_attributes};

    #[test]
    fn previous_attributes_hold_the_former_values_of_changed_fields() {
        let before = json!({
            "url": "https://a.example",
            "status": "enabled",
            "created": 1,
            "enabled_events": ["deposit.credited"],
            "metadata": {"order": "1", "kept": "x", "dropped": "y"},
        });
        let after = json!({
            "url": "https://b.example",
            "status": "enabled",
            "created": 1,
            "enabled_events": ["deposit.credited", "refund.failed"],
            "metadata": {"order": "2", "kept": "x", "added": "z"},
            "deleted": true,
        });
        assert_eq!(
            previous_attributes(&before, &after),
            json!({
                "url": "https://a.example",
                "enabled_events": ["deposit.credited"],
                "metadata": {"order": "1", "dropped": "y", "added": null},
                "deleted": null,
            })
        );
        assert_eq!(previous_attributes(&before, &before), json!({}));
    }

    #[test]
    fn account_events_are_the_account_key_and_endpoint_changes() {
        for event_type in [
            "account.updated",
            "treasury.created",
            "treasury.canceled",
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
