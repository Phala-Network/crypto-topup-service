//! Webhook endpoints (`/v1/webhook_endpoints`, design D11, §11): the merchant registers,
//! changes, tests, and deletes its receivers with a secret key, Stripe's endpoints API.
//!
//! The service checks only a URL's form, scheme, and port (`https` on 443; in test mode also
//! `http` on 80). Which addresses it may reach is decided by the egress proxy every delivery goes
//! through, smokescreen, the only IP filter (design §8): a URL is never resolved here.

use axum::Json;
use axum::extract::{Extension, RawQuery, State};

use crate::ids;
use crate::tenancy::Permission;
use crate::webhook_endpoints::{self, Changes, EndpointError, NewEndpoint, Page};

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, query_pairs};
use super::metadata::{self, MetadataUpdate};
use super::models::{
    CreateWebhookEndpointRequest, DeletedWebhookEndpoint, EventObjectResponse,
    UpdateWebhookEndpointRequest, WebhookEndpointList, WebhookEndpointObject,
};

type ApiResult<T> = Result<T, ApiError>;

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;
const MAX_URL_BYTES: usize = 2048;
const MAX_DESCRIPTION_CHARS: usize = 5000;

#[utoipa::path(
    post,
    path = "/v1/webhook_endpoints",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateWebhookEndpointRequest,
    responses(
        (status = 200, description = "OK", body = WebhookEndpointObject),
        (status = 400, description = "Bad Request, or `webhook_endpoint_cap_exceeded`: the mode already has 16 endpoints", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// Registers a webhook endpoint in the key's mode, at most 16 per mode. It receives the events it
/// subscribes to and, whatever it subscribes to, every account event (`account.*`, `api_key.*`,
/// `webhook_endpoint.*`), starting with `webhook_endpoint.created` about itself. Deliveries are
/// signed with the account's webhook key of the mode (`GET /v1/attestation`).
pub(crate) async fn create_webhook_endpoint(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateWebhookEndpointRequest>,
) -> ApiResult<Json<WebhookEndpointObject>> {
    merchant
        .require(&state.pool, Permission::EndpointsWrite)
        .await?;
    let livemode = merchant.scope.livemode();
    validate_url(&request.url, livemode, local_stack(&state))?;
    let enabled_events = validate_enabled_events(&request.enabled_events)?;
    let description = description(request.description.as_deref())?;
    let metadata = metadata::on_create(request.metadata.as_ref())?;
    let endpoint = webhook_endpoints::create(
        &state.pool,
        merchant.scope,
        &NewEndpoint {
            url: &request.url,
            enabled_events: &enabled_events,
            description,
            metadata,
        },
        &merchant.actor(),
    )
    .await
    .map_err(map_error)?;
    rendered(&state, &endpoint).await
}

#[utoipa::path(
    get,
    path = "/v1/webhook_endpoints",
    params(
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10"),
        ("starting_after" = Option<String>, Query, description = "`we_` id: the page after it"),
        ("ending_before" = Option<String>, Query, description = "`we_` id: the page before it")
    ),
    responses(
        (status = 200, description = "OK", body = WebhookEndpointList),
        (status = 400, description = "Bad Request, or `webhook_endpoint_cap_exceeded`: the mode already has 16 endpoints", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// The key's mode's webhook endpoints, newest first, with Stripe's cursor pagination.
pub(crate) async fn list_webhook_endpoints(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<WebhookEndpointList>> {
    merchant
        .require(&state.pool, Permission::EndpointsRead)
        .await?;
    let page = parse_page(&query_pairs(query.as_deref()))?;
    let (endpoints, has_more) = webhook_endpoints::list(&state.pool, merchant.scope, page)
        .await
        .map_err(|error| match error {
            EndpointError::NotFound => ApiError::invalid_param(
                if page.before {
                    "ending_before"
                } else {
                    "starting_after"
                },
                "no such webhook endpoint",
            ),
            error => map_error(error),
        })?;
    let ids: Vec<_> = endpoints.iter().map(|endpoint| endpoint.id).collect();
    let mut backlogs = webhook_endpoints::backlogs(&state.pool, &ids).await?;
    Ok(Json(WebhookEndpointList {
        object: "list".to_owned(),
        url: "/v1/webhook_endpoints".to_owned(),
        has_more,
        data: endpoints
            .iter()
            .map(|endpoint| endpoint.object(backlogs.remove(&endpoint.id).unwrap_or_default()))
            .collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/v1/webhook_endpoints/{id}",
    params(("id" = String, Path, description = "Endpoint id, `we_…`")),
    responses(
        (status = 200, description = "OK", body = WebhookEndpointObject),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// One webhook endpoint.
pub(crate) async fn get_webhook_endpoint(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<WebhookEndpointObject>> {
    merchant
        .require(&state.pool, Permission::EndpointsRead)
        .await?;
    let id = ids::parse(ids::WEBHOOK_ENDPOINT, &id).ok_or_else(ApiError::not_found)?;
    let endpoint = webhook_endpoints::get(&state.pool, merchant.scope, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    rendered(&state, &endpoint).await
}

/// The endpoint's API representation with its delivery health.
async fn rendered(
    state: &AppState,
    endpoint: &webhook_endpoints::WebhookEndpoint,
) -> ApiResult<Json<WebhookEndpointObject>> {
    let mut connection = state.pool.acquire().await?;
    Ok(Json(
        webhook_endpoints::render(&mut connection, endpoint).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/webhook_endpoints/{id}",
    params(
        ("id" = String, Path, description = "Endpoint id, `we_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = UpdateWebhookEndpointRequest,
    responses(
        (status = 200, description = "OK", body = WebhookEndpointObject),
        (status = 400, description = "Bad Request, or `webhook_endpoint_cap_exceeded`: the mode already has 16 endpoints", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// Updates a webhook endpoint; parameters not sent are left unchanged. A change is announced as
/// `webhook_endpoint.updated`, with the replaced values in `data.previous_attributes`, to every
/// enabled endpoint, and first to this endpoint at the URL it had before, even when the change
/// disables it. Disabling stops its pending deliveries; enabling does not restart them (resend
/// with `POST /v1/events/{id}/resend`).
pub(crate) async fn update_webhook_endpoint(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
    ApiJson(request): ApiJson<UpdateWebhookEndpointRequest>,
) -> ApiResult<Json<WebhookEndpointObject>> {
    merchant
        .require(&state.pool, Permission::EndpointsWrite)
        .await?;
    let id = ids::parse(ids::WEBHOOK_ENDPOINT, &id).ok_or_else(ApiError::not_found)?;
    if let Some(url) = &request.url {
        validate_url(url, merchant.scope.livemode(), local_stack(&state))?;
    }
    let enabled_events = request
        .enabled_events
        .as_deref()
        .map(validate_enabled_events)
        .transpose()?;
    let description = request
        .description
        .as_deref()
        .map(|value| description(Some(value)))
        .transpose()?;
    let metadata = request
        .metadata
        .as_ref()
        .map(MetadataUpdate::parse)
        .transpose()?;
    let endpoint = webhook_endpoints::update(
        &state.pool,
        merchant.scope,
        id,
        &Changes {
            url: request.url.as_deref(),
            enabled_events: enabled_events.as_deref(),
            description,
            disabled: request.disabled,
            metadata: metadata.as_ref(),
        },
        &merchant.actor(),
    )
    .await
    .map_err(map_error)?;
    rendered(&state, &endpoint).await
}

#[utoipa::path(
    delete,
    path = "/v1/webhook_endpoints/{id}",
    params(("id" = String, Path, description = "Endpoint id, `we_…`")),
    responses(
        (status = 200, description = "OK", body = DeletedWebhookEndpoint),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// Deletes a webhook endpoint. `webhook_endpoint.deleted` goes to every enabled endpoint, and
/// first to the deleted one; then it receives nothing more, and its pending deliveries stop.
pub(crate) async fn delete_webhook_endpoint(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<DeletedWebhookEndpoint>> {
    merchant
        .require(&state.pool, Permission::EndpointsWrite)
        .await?;
    let id = ids::parse(ids::WEBHOOK_ENDPOINT, &id).ok_or_else(ApiError::not_found)?;
    let endpoint = webhook_endpoints::delete(&state.pool, merchant.scope, id, &merchant.actor())
        .await
        .map_err(map_error)?;
    Ok(Json(DeletedWebhookEndpoint {
        id: endpoint.public_id(),
        object: "webhook_endpoint".to_owned(),
        deleted: true,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/webhook_endpoints/{id}/test",
    params(
        ("id" = String, Path, description = "Endpoint id, `we_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    responses(
        (status = 200, description = "OK: the test event, queued", body = EventObjectResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "webhook_endpoints"
)]
/// Sends a `webhook_endpoint.test` event about the endpoint to this endpoint only, enabled or
/// not, signed like every delivery: check your receiver and its signature verification with it.
/// There is no URL challenge.
pub(crate) async fn test_webhook_endpoint(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<EventObjectResponse>> {
    merchant
        .require(&state.pool, Permission::EndpointsWrite)
        .await?;
    let id = ids::parse(ids::WEBHOOK_ENDPOINT, &id).ok_or_else(ApiError::not_found)?;
    let event_id = webhook_endpoints::send_test(&state.pool, merchant.scope, id, &merchant.actor())
        .await
        .map_err(map_error)?;
    super::events::find_event(&state, merchant.scope, event_id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::internal)
}

/// Whether the service's own public origin is `http`, which only local stacks use: there any
/// scheme and port is accepted, since the local receivers listen on compose-network ports.
fn local_stack(state: &AppState) -> bool {
    state.public_origin.to_string().starts_with("http://")
}

/// Requires an absolute URL without credentials or fragment, `https` on port 443 or, in test
/// mode, `http` on port 80 (design §8); a local stack accepts any `http` or `https` port.
fn validate_url(value: &str, livemode: bool, local_stack: bool) -> ApiResult<()> {
    let invalid = |message: &str| ApiError::invalid_param("url", message);
    if value.len() > MAX_URL_BYTES {
        return Err(invalid("url must be at most 2048 bytes"));
    }
    let url = url::Url::parse(value).map_err(|_| invalid("url must be an absolute URL"))?;
    if url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid(
            "url must name a host and carry no credentials or fragment",
        ));
    }
    let allowed = match (url.scheme(), url.port_or_known_default()) {
        _ if local_stack => matches!(url.scheme(), "http" | "https"),
        ("https", Some(443)) => true,
        ("http", Some(80)) => !livemode,
        _ => false,
    };
    if !allowed {
        return Err(invalid(if livemode {
            "url must use https on port 443 in live mode"
        } else {
            "url must use https on port 443, or http on port 80"
        }));
    }
    Ok(())
}

/// `["*"]` or known event types, without repeats.
fn validate_enabled_events(events: &[String]) -> ApiResult<Vec<String>> {
    let invalid = |message: String| ApiError::invalid_param("enabled_events", message);
    if events.is_empty() {
        return Err(invalid(
            "enabled_events must list at least one event type".to_owned(),
        ));
    }
    if events.iter().any(|event| event == "*") {
        return if events.len() == 1 {
            Ok(events.to_vec())
        } else {
            Err(invalid(
                "`*` cannot be combined with other event types".to_owned(),
            ))
        };
    }
    let mut validated = Vec::with_capacity(events.len());
    for event in events {
        if !webhook_endpoints::EVENT_TYPES.contains(&event.as_str()) {
            return Err(invalid(format!("unknown event type `{event}`")));
        }
        if !validated.contains(event) {
            validated.push(event.clone());
        }
    }
    Ok(validated)
}

/// A description of 1 to 5000 characters; `""` is none.
fn description(value: Option<&str>) -> ApiResult<Option<&str>> {
    match value {
        Some(value) if value.chars().count() > MAX_DESCRIPTION_CHARS => Err(
            ApiError::invalid_param("description", "description must be at most 5000 characters"),
        ),
        Some("") | None => Ok(None),
        Some(value) => Ok(Some(value)),
    }
}

fn parse_page(pairs: &[(String, String)]) -> ApiResult<Page> {
    let mut page = Page {
        limit: DEFAULT_LIMIT,
        ..Page::default()
    };
    let mut starting_after = None;
    let mut ending_before = None;
    for (name, value) in pairs {
        match name.as_str() {
            "limit" => {
                page.limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            "starting_after" | "ending_before" => {
                let id = ids::parse(ids::WEBHOOK_ENDPOINT, value)
                    .ok_or_else(|| ApiError::invalid_param(name.clone(), "not a we_ id"))?;
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
    match (starting_after, ending_before) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "starting_after and ending_before are mutually exclusive",
            ));
        }
        (Some(id), None) => page.cursor = Some(id),
        (None, Some(id)) => {
            page.cursor = Some(id);
            page.before = true;
        }
        (None, None) => {}
    }
    Ok(page)
}

pub(super) fn map_error(error: EndpointError) -> ApiError {
    match error {
        EndpointError::NotFound => ApiError::not_found(),
        EndpointError::EventNotFound => ApiError::not_found(),
        EndpointError::Disabled => ApiError::webhook_endpoint_disabled(),
        EndpointError::LimitReached => ApiError::webhook_endpoint_cap(),
        EndpointError::Metadata(error) => error,
        EndpointError::Serialization => ApiError::internal(),
        EndpointError::Database(error) => error.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_https_on_443_or_http_on_80_in_test_mode() {
        for (url, livemode) in [
            ("https://merchant.example/webhooks", true),
            ("https://merchant.example:443/webhooks", true),
            ("https://merchant.example/webhooks", false),
            ("http://merchant.example/webhooks", false),
        ] {
            assert!(validate_url(url, livemode, false).is_ok(), "{url}");
        }
        for (url, livemode) in [
            ("http://merchant.example/webhooks", true),
            ("https://merchant.example:8443/webhooks", true),
            ("http://merchant.example:8080/webhooks", false),
            ("https://merchant.example:80/webhooks", false),
            ("ftp://merchant.example/webhooks", false),
            ("merchant.example/webhooks", false),
            ("https://user@merchant.example/webhooks", false),
            ("https://merchant.example/webhooks#fragment", false),
        ] {
            assert!(validate_url(url, livemode, false).is_err(), "{url}");
        }
        // A local stack's receivers listen on compose-network ports.
        assert!(validate_url("http://product:8089/webhooks", true, true).is_ok());
        assert!(validate_url("ftp://product:8089/webhooks", true, true).is_err());
    }

    #[test]
    fn enabled_events_are_a_wildcard_or_known_types() {
        let list = |events: &[&str]| {
            events
                .iter()
                .map(|event| (*event).to_owned())
                .collect::<Vec<_>>()
        };
        assert_eq!(
            validate_enabled_events(&list(&["*"])).ok(),
            Some(list(&["*"]))
        );
        assert_eq!(
            validate_enabled_events(&list(&["deposit.credited", "deposit.credited"])).ok(),
            Some(list(&["deposit.credited"]))
        );
        for events in [
            list(&[]),
            list(&["*", "deposit.credited"]),
            list(&["deposit.*"]),
            list(&["webhook_endpoint.test"]),
        ] {
            assert!(validate_enabled_events(&events).is_err(), "{events:?}");
        }
    }
}
