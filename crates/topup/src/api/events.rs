//! Events (`/v1/events`, design §13): every event of the account in the key's mode, newest
//! first, Stripe's Events API. They are the merchant's notifications and its audit log: each
//! names its `actor`. `POST /v1/events/{id}/resend` delivers one again to an endpoint.

use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use axum::response::Response;
use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::{FromRow, PgExecutor, Postgres, QueryBuilder};
use uuid::Uuid;

use crate::ids;
use crate::outbox::webhook_id;
use crate::tenancy::Scope;
use crate::webhook_endpoints;

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, query_pairs};
use super::idempotency::Idempotent;
use super::models::{EventData, EventList, EventObjectResponse, EventRequest, ResendEventRequest};

type ApiResult<T> = Result<T, ApiError>;

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;

#[utoipa::path(
    get,
    path = "/v1/events",
    params(
        (
            "type" = Option<String>, Query,
            description = "Only events of this type, such as `deposit.credited`, or of a group, \
                           such as `deposit.*`"
        ),
        (
            "types[]" = Option<Vec<String>>, Query,
            description = "Only events of these types, up to 20, each a type or a group; not with \
                           `type` (<https://docs.stripe.com/api/events/list>)"
        ),
        (
            "delivery_success" = Option<bool>, Query,
            description = "`false`: only events with a delivery to a webhook endpoint that has \
                           not succeeded, pending or stopped; `true`: only events whose every \
                           delivery succeeded"
        ),
        ("created[gt]" = Option<i64>, Query, description = "Created after, Unix seconds"),
        ("created[gte]" = Option<i64>, Query, description = "Created at or after, Unix seconds"),
        ("created[lt]" = Option<i64>, Query, description = "Created before, Unix seconds"),
        ("created[lte]" = Option<i64>, Query, description = "Created at or before, Unix seconds"),
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10"),
        ("starting_after" = Option<String>, Query, description = "`evt_` id: the page after it"),
        ("ending_before" = Option<String>, Query, description = "`evt_` id: the page before it")
    ),
    responses(
        (status = 200, description = "OK", body = EventList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "events"
)]
/// The account's events in the key's mode, newest first, with Stripe's cursor pagination: the
/// notifications webhooks deliver, and the audit log of every key, endpoint, and account change
/// with its `actor`. An event stays listed whether or not any endpoint received it.
pub(crate) async fn list_events(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<EventList>> {
    let filter = parse_filter(&query_pairs(query.as_deref()))?;
    let mut builder = select(merchant.scope);
    if !filter.types.is_empty() {
        builder.push(" AND (");
        for (index, event_type) in filter.types.iter().enumerate() {
            if index > 0 {
                builder.push(" OR ");
            }
            match event_type.strip_suffix('*') {
                Some(prefix) => builder
                    .push("starts_with(event.type, ")
                    .push_bind(prefix.to_owned())
                    .push(")"),
                None => builder.push("event.type = ").push_bind(event_type.clone()),
            };
        }
        builder.push(")");
    }
    if let Some(success) = filter.delivery_success {
        builder.push(if success {
            " AND NOT EXISTS ("
        } else {
            " AND EXISTS ("
        });
        builder.push(
            "SELECT 1 FROM webhook_deliveries AS delivery \
             WHERE delivery.event_id = event.id AND delivery.delivered_at IS NULL)",
        );
    }
    for (operator, bound) in &filter.created {
        builder
            .push(format!(" AND event.created {operator} "))
            .push_bind(*bound);
    }
    if let Some(cursor) = filter.cursor {
        let found: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(
            "SELECT created, id FROM events WHERE id = $1 AND account_id = $2 AND livemode = $3",
        )
        .bind(cursor)
        .bind(merchant.scope.account_id())
        .bind(merchant.scope.livemode())
        .fetch_optional(&state.pool)
        .await?;
        let (created, id) = found.ok_or_else(|| {
            ApiError::invalid_param(
                if filter.before {
                    "ending_before"
                } else {
                    "starting_after"
                },
                "no such event",
            )
        })?;
        builder
            .push(if filter.before {
                " AND (event.created, event.id) > ("
            } else {
                " AND (event.created, event.id) < ("
            })
            .push_bind(created)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    builder.push(if filter.before {
        " ORDER BY event.created ASC, event.id ASC LIMIT "
    } else {
        " ORDER BY event.created DESC, event.id DESC LIMIT "
    });
    builder.push_bind(filter.limit.saturating_add(1));
    let mut rows = builder
        .build_query_as::<EventRow>()
        .fetch_all(&state.pool)
        .await?;
    let limit = usize::try_from(filter.limit).map_err(|_| ApiError::internal())?;
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    if filter.before {
        rows.reverse();
    }
    let data = rows
        .into_iter()
        .map(event_object)
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(EventList {
        object: "list".to_owned(),
        url: "/v1/events".to_owned(),
        has_more,
        data,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/events/{id}",
    params(("id" = String, Path, description = "Event id, `evt_…`")),
    responses(
        (status = 200, description = "OK", body = EventObjectResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "events"
)]
/// One event.
pub(crate) async fn get_event(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<EventObjectResponse>> {
    let id = ids::parse(ids::EVENT, &id).ok_or_else(ApiError::not_found)?;
    find_event(&state.pool, merchant.scope, id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

#[utoipa::path(
    post,
    path = "/v1/events/{id}/resend",
    params(
        ("id" = String, Path, description = "Event id, `evt_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = ResendEventRequest,
    responses(
        (status = 200, description = "OK: queued for delivery", body = EventObjectResponse),
        (
            status = 400,
            description = "Bad Request, or `webhook_endpoint_disabled`: enable the endpoint first",
            body = ErrorResponse
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found: no such event or endpoint", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "events"
)]
/// Delivers an event again to one enabled endpoint, whether it was delivered there, stopped, or
/// never sent there (the Stripe CLI's `events resend`), with the same `webhook-id` and body. Use
/// it after re-enabling an endpoint for the events it missed.
pub(crate) async fn resend_event(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    idempotent: Idempotent,
    ApiPath(id): ApiPath<String>,
    ApiJson(request): ApiJson<ResendEventRequest>,
) -> ApiResult<Response> {
    let event_id = ids::parse(ids::EVENT, &id).ok_or_else(ApiError::not_found)?;
    let endpoint_id = ids::parse(ids::WEBHOOK_ENDPOINT, &request.webhook_endpoint)
        .ok_or_else(|| ApiError::not_found().with_param("webhook_endpoint"))?;
    let mut transaction = idempotent.begin(&state.pool).await?;
    webhook_endpoints::resend(
        &mut *transaction,
        merchant.scope,
        event_id,
        endpoint_id,
        &merchant.actor(),
    )
    .await
    .map_err(|error| match error {
        webhook_endpoints::EndpointError::NotFound => {
            ApiError::not_found().with_param("webhook_endpoint")
        }
        error => super::webhook_endpoints::map_error(error),
    })?;
    let event = find_event(&mut *transaction, merchant.scope, event_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    idempotent.commit(transaction, Json(event)).await
}

/// The scope's event `id` as the API shows it.
pub(crate) async fn find_event<'e>(
    executor: impl PgExecutor<'e>,
    scope: Scope,
    id: Uuid,
) -> ApiResult<Option<EventObjectResponse>> {
    let mut builder = select(scope);
    builder.push(" AND event.id = ").push_bind(id);
    let row = builder
        .build_query_as::<EventRow>()
        .fetch_optional(executor)
        .await?;
    row.map(event_object).transpose()
}

#[derive(FromRow)]
struct EventRow {
    id: Uuid,
    public_id: String,
    livemode: bool,
    #[sqlx(rename = "type")]
    event_type: String,
    actor: String,
    request_id: Option<String>,
    idempotency_key: Option<String>,
    data: Value,
    created: DateTime<Utc>,
    pending_webhooks: i64,
}

fn select(scope: Scope) -> QueryBuilder<Postgres> {
    let mut builder = QueryBuilder::new(
        r#"
        SELECT event.id, account.public_id, event.livemode, event.type, event.actor,
               event.request_id, event.idempotency_key, event.data, event.created,
               (SELECT count(*) FROM webhook_deliveries AS delivery
                WHERE delivery.event_id = event.id
                  AND delivery.delivered_at IS NULL AND delivery.failed_at IS NULL)
                   AS pending_webhooks
        FROM events AS event
        JOIN accounts AS account ON account.id = event.account_id
        WHERE event.account_id = "#,
    );
    builder
        .push_bind(scope.account_id())
        .push(" AND event.livemode = ")
        .push_bind(scope.livemode());
    builder
}

/// The row as the API shows it: its `data` as recorded with the change.
fn event_object(row: EventRow) -> ApiResult<EventObjectResponse> {
    let data = serde_json::from_value::<EventData>(row.data).map_err(|error| {
        tracing::error!(event_id = %crate::ids::format(crate::ids::EVENT, row.id), %error, "a stored event has no object");
        ApiError::internal()
    })?;
    Ok(EventObjectResponse {
        id: webhook_id(row.id),
        object: "event".to_owned(),
        account: row.public_id,
        livemode: row.livemode,
        event_type: row.event_type,
        created: row.created.timestamp(),
        actor: row.actor,
        request: row.request_id.map(|id| EventRequest {
            id,
            idempotency_key: row.idempotency_key,
        }),
        data,
        pending_webhooks: row.pending_webhooks,
    })
}

/// At most this many `types[]`, Stripe's limit.
const MAX_TYPES: usize = 20;

#[derive(Default)]
struct Filter {
    types: Vec<String>,
    delivery_success: Option<bool>,
    created: Vec<(&'static str, DateTime<Utc>)>,
    limit: i64,
    cursor: Option<Uuid>,
    before: bool,
}

fn parse_filter(pairs: &[(String, String)]) -> ApiResult<Filter> {
    let mut filter = Filter {
        limit: DEFAULT_LIMIT,
        ..Filter::default()
    };
    let mut starting_after = None;
    let mut ending_before = None;
    let mut single_type = false;
    for (name, value) in pairs {
        match name.as_str() {
            "type" | "types[]" => {
                if !valid_type_filter(value) {
                    return Err(ApiError::invalid_param(
                        name.clone(),
                        "each type must be an event type, such as deposit.credited, or a \
                         group, such as deposit.*",
                    ));
                }
                single_type |= name == "type";
                filter.types.push(value.clone());
            }
            "delivery_success" => {
                filter.delivery_success = Some(match value.as_str() {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(ApiError::invalid_param(
                            "delivery_success",
                            "delivery_success must be true or false",
                        ));
                    }
                });
            }
            "created[gt]" | "created[gte]" | "created[lt]" | "created[lte]" => {
                filter
                    .created
                    .extend(super::pagination::created_bound(name, value)?);
            }
            "limit" => {
                filter.limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            "starting_after" | "ending_before" => {
                let id = ids::parse(ids::EVENT, value)
                    .ok_or_else(|| ApiError::invalid_param(name.clone(), "not an evt_ id"))?;
                if name == "starting_after" {
                    starting_after = Some(id);
                } else {
                    ending_before = Some(id);
                }
            }
            other => {
                return Err(
                    ApiError::unknown_param(format!("unknown parameter {other}")).with_param(other),
                );
            }
        }
    }
    if single_type && filter.types.len() > 1 {
        return Err(ApiError::invalid_param(
            "types[]",
            "send one type, or several as types[], not both",
        ));
    }
    if filter.types.len() > MAX_TYPES {
        return Err(ApiError::invalid_param(
            "types[]",
            format!("types[] takes at most {MAX_TYPES} types"),
        ));
    }
    match (starting_after, ending_before) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "starting_after and ending_before are mutually exclusive",
            ));
        }
        (Some(id), None) => filter.cursor = Some(id),
        (None, Some(id)) => {
            filter.cursor = Some(id);
            filter.before = true;
        }
        (None, None) => {}
    }
    Ok(filter)
}

/// `word(.word)*`, optionally ending in `.*`, of lowercase letters and underscores.
fn valid_type_filter(value: &str) -> bool {
    let name = value.strip_suffix(".*").unwrap_or(value);
    value.len() <= 100
        && !name.is_empty()
        && name.split('.').all(|word| {
            !word.is_empty()
                && word
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        })
}

#[cfg(test)]
mod tests {
    use super::valid_type_filter;

    #[test]
    fn type_filters_are_a_type_or_a_group() {
        for value in ["deposit.credited", "deposit.*", "treasury.*", "account"] {
            assert!(valid_type_filter(value), "{value}");
        }
        for value in [
            "",
            "*",
            ".*",
            "deposit.",
            "deposit..credited",
            "Deposit.x",
            "deposit.*.x",
        ] {
            assert!(!valid_type_filter(value), "{value}");
        }
    }
}
