//! Typed API handlers for the routes in architecture §12.

use std::str::FromStr;

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::B256;
use axum::Json;
use axum::extract::{Path, Query, State};
use topup_core::screening::PauseScope;
use uuid::Uuid;

use crate::db::{Account, Product};
use crate::routes::{ProviderError, RouteSet};

use super::AppState;
use super::attestation::AttestationError;
use super::auth::VerificationKey;
use super::error::{ApiError, ErrorResponse};
use super::models::{
    AdminReasonRequest, AdminRefundResponse, AttestationQuery, AttestationResponse,
    DailyReportResponse, NudgeResponse, OutboxReplayResponse, PauseRequest, PauseResponse,
    ProductResponse, ReconciliationBlockLiftResponse, RecordRefundRequest, RegisterProductRequest,
    RoutePauseResponse, SupportDepositResponse, UpdateProductRequest,
};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    get,
    path = "/v1/attestation",
    params(AttestationQuery),
    responses(
        (status = 200, description = "OK", body = AttestationResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 503, description = "Service Unavailable", body = ErrorResponse)
    ),
    tag = "attestation"
)]
pub(crate) async fn get_attestation(
    State(state): State<AppState>,
    Query(query): Query<AttestationQuery>,
) -> ApiResult<Json<AttestationResponse>> {
    let nonce = decode_nonce(&query.nonce)?;
    // The flusher signs with these keys; startup refuses routes that disagree on one.
    let operator_keys = state.routes.operator_keys().map_err(|error| {
        tracing::error!(%error, "invalid operator key configuration");
        ApiError::service_unavailable("attestation is unavailable")
    })?;
    match state.attestor.attest(&nonce, &operator_keys).await {
        Ok(response) => Ok(Json(response)),
        Err(AttestationError::Unavailable) => {
            Err(ApiError::service_unavailable("attestation is unavailable"))
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/admin/products",
    request_body = RegisterProductRequest,
    responses(
        (status = 200, description = "OK: registered, or already registered with the same values", body = ProductResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 409, description = "Conflict: the slug is registered with different values; `PUT /v1/admin/products/{slug}` replaces them", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn register_product(
    State(state): State<AppState>,
    Json(request): Json<RegisterProductRequest>,
) -> ApiResult<Json<ProductResponse>> {
    validate_product_credentials(
        &state,
        &request.slug,
        &request.public_key,
        &request.webhook_url,
    )?;
    let product = repository::register_product(
        &state.pool,
        &request.slug,
        &request.public_key,
        &request.webhook_url,
        &admin_actor(&state),
    )
    .await?;
    Ok(Json(product_response(product)))
}

#[utoipa::path(
    put,
    path = "/v1/admin/products/{slug}",
    params(("slug" = String, Path, description = "Product slug")),
    request_body = UpdateProductRequest,
    responses(
        (status = 200, description = "OK: replaced, or already holding these values", body = ProductResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found: no product is issued with this slug", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn update_product(
    State(state): State<AppState>,
    Path(slug): Path<String>,
    Json(request): Json<UpdateProductRequest>,
) -> ApiResult<Json<ProductResponse>> {
    validate_product_credentials(&state, &slug, &request.public_key, &request.webhook_url)?;
    validate_reason(&request.reason)?;
    let product = repository::update_product(
        &state.pool,
        &slug,
        &request.public_key,
        &request.webhook_url,
        &admin_actor(&state),
        &request.reason,
    )
    .await?;
    Ok(Json(product_response(product)))
}

#[utoipa::path(
    get,
    path = "/v1/admin/deposits/{id}",
    params(("id" = String, Path, description = "Deposit id, `dep_…` or the UUID")),
    responses(
        (status = 200, description = "OK", body = SupportDepositResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// One deposit of any product with its stored facts, transitions, and webhook events.
pub(crate) async fn admin_get_deposit(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> ApiResult<Json<SupportDepositResponse>> {
    let id = crate::ids::parse(crate::ids::DEPOSIT, &id)
        .or_else(|| Uuid::parse_str(&id).ok())
        .ok_or_else(ApiError::not_found)?;
    repository::admin_deposit(&state.pool, id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

#[utoipa::path(
    post,
    path = "/v1/admin/products/{slug}/accounts/{account_id}/pause",
    params(("slug" = String, Path), ("account_id" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Pauses scopes of one product account, for example `settlement` to stop crediting it.
pub(crate) async fn pause_account(
    State(state): State<AppState>,
    Path((slug, account_id)): Path<(String, String)>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &slug, &account_id, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/products/{slug}/accounts/{account_id}/resume",
    params(("slug" = String, Path), ("account_id" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Resumes scopes of one product account.
pub(crate) async fn resume_account(
    State(state): State<AppState>,
    Path((slug, account_id)): Path<(String, String)>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &slug, &account_id, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/routes/{r}/pause",
    params(("r" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = RoutePauseResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn pause_route(
    State(state): State<AppState>,
    Path(route): Path<String>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<RoutePauseResponse>> {
    mutate_route_scopes(&state, &route, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/routes/{r}/resume",
    params(("r" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = RoutePauseResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn resume_route(
    State(state): State<AppState>,
    Path(route): Path<String>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<RoutePauseResponse>> {
    mutate_route_scopes(&state, &route, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/deposits/{id}/nudge",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "OK", body = NudgeResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn nudge_deposit(
    State(state): State<AppState>,
    Path(deposit_id): Path<Uuid>,
) -> ApiResult<Json<NudgeResponse>> {
    Ok(Json(
        repository::nudge_deposit(&state.pool, deposit_id, &admin_actor(&state)).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/approve",
    params(("id" = Uuid, Path)),
    responses((status = 200, description = "OK", body = AdminRefundResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn approve_refund(
    State(state): State<AppState>,
    Path(refund_id): Path<Uuid>,
) -> ApiResult<Json<AdminRefundResponse>> {
    Ok(Json(
        repository::approve_refund(
            &state.pool,
            refund_id,
            state.routes.routes(),
            &admin_actor(&state),
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/record",
    params(("id" = Uuid, Path)),
    request_body = RecordRefundRequest,
    responses(
        (status = 200, description = "OK", body = AdminRefundResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (status = 409, description = "Conflict", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn record_refund(
    State(state): State<AppState>,
    Path(refund_id): Path<Uuid>,
    Json(request): Json<RecordRefundRequest>,
) -> ApiResult<Json<AdminRefundResponse>> {
    let tx_hash = B256::from_str(&request.tx_hash)
        .map_err(|_| ApiError::bad_request("tx_hash must be a 32-byte hexadecimal value"))?;
    Ok(Json(
        repository::record_refund(&state.pool, refund_id, tx_hash, &admin_actor(&state)).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/reconciliation-blocks/{block_key}/lift",
    params(("block_key" = String, Path, description = "`chain:{chain_id}` or `address:{address_id}`, as listed in the daily report")),
    request_body = AdminReasonRequest,
    responses(
        (status = 200, description = "OK: lifted, or already lifted", body = ReconciliationBlockLiftResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found: no active or lifted block has this key", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn lift_reconciliation_block(
    State(state): State<AppState>,
    Path(block_key): Path<String>,
    Json(request): Json<AdminReasonRequest>,
) -> ApiResult<Json<ReconciliationBlockLiftResponse>> {
    validate_reason(&request.reason)?;
    Ok(Json(
        repository::lift_reconciliation_block(
            &state.pool,
            &block_key,
            &admin_actor(&state),
            &request.reason,
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/outbox/{event_id}/replay",
    params((
        "event_id" = String,
        Path,
        description = "The `webhook-id` header: `evt_…`, or the UUID of an older event"
    )),
    request_body = AdminReasonRequest,
    responses(
        (status = 200, description = "OK: queued for delivery", body = OutboxReplayResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn replay_outbox_event(
    State(state): State<AppState>,
    Path(event_id): Path<String>,
    Json(request): Json<AdminReasonRequest>,
) -> ApiResult<Json<OutboxReplayResponse>> {
    let event_id = crate::ids::parse_event(&event_id).ok_or_else(ApiError::not_found)?;
    validate_reason(&request.reason)?;
    Ok(Json(
        repository::replay_outbox_event(
            &state.pool,
            event_id,
            &admin_actor(&state),
            &request.reason,
        )
        .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/admin/report/daily",
    responses((status = 200, description = "OK", body = DailyReportResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn daily_report(
    State(state): State<AppState>,
) -> ApiResult<Json<DailyReportResponse>> {
    let mut report =
        repository::daily_report(&state.pool, state.routes.routes(), chrono::Utc::now()).await?;
    populate_treasury_balances(&state.routes, &mut report).await;
    for route_report in &mut report.routes {
        route_report.flush_planning =
            crate::observability::flush_planning(&route_report.route).map(Into::into);
    }
    report.reconciliation = crate::observability::reconciliation().map(Into::into);
    Ok(Json(report))
}

async fn populate_treasury_balances(routes: &RouteSet, report: &mut DailyReportResponse) {
    for route_report in &mut report.routes {
        let Some(route) = routes
            .routes()
            .iter()
            .filter(|route| route.route == route_report.route)
            .max_by_key(|route| route.version)
        else {
            continue;
        };
        let chain_id = route.chain.chain_id;
        let client = match routes.provider(chain_id, 0) {
            Ok(client) => client,
            Err(ProviderError::MissingUrl { environment, .. }) => {
                route_report.treasury_balance_note =
                    format!("treasury balance unavailable: {environment} is not configured");
                continue;
            }
            Err(ProviderError::Unconfigured { .. }) => {
                route_report.treasury_balance_note =
                    "treasury balance unavailable: route has no RPC provider".to_owned();
                continue;
            }
            Err(ProviderError::InvalidKey { environment, .. }) => {
                route_report.treasury_balance_note =
                    format!("treasury balance unavailable: {environment} does not fit its URL");
                continue;
            }
            Err(ProviderError::InvalidUrl { .. }) => {
                route_report.treasury_balance_note =
                    "treasury balance unavailable: RPC client configuration is invalid".to_owned();
                continue;
            }
        };
        match client
            .token_balances(
                route.asset.contract,
                &[route.chain.contracts.treasury],
                BlockNumberOrTag::Latest,
            )
            .await
        {
            Ok(balances) => match balances.into_iter().next() {
                Some(balance) => {
                    route_report.treasury_balance_atomic = Some(balance.to_string());
                    route_report.treasury_balance_note =
                        "latest on-chain ERC-20 treasury balance".to_owned();
                }
                None => {
                    route_report.treasury_balance_note =
                        "treasury balance unavailable: RPC returned no balance".to_owned();
                }
            },
            Err(error) => {
                tracing::warn!(route = %route.route, %error, "daily report treasury balance read failed");
                route_report.treasury_balance_note =
                    "treasury balance unavailable: RPC read failed".to_owned();
            }
        }
    }
}

/// Finds or creates the account, so creating a quote or an address is one call.
pub(super) async fn ensure_account(
    state: &AppState,
    product_id: Uuid,
    external_id: &str,
) -> ApiResult<Account> {
    validate_external_id(external_id)?;
    repository::register_account(&state.pool, product_id, external_id).await
}

pub(super) async fn require_account(
    state: &AppState,
    product_id: Uuid,
    external_id: &str,
) -> ApiResult<Account> {
    repository::find_account(&state.pool, product_id, external_id)
        .await?
        .ok_or_else(ApiError::not_found)
}

async fn mutate_account_scopes(
    state: &AppState,
    slug: &str,
    external_id: &str,
    request: PauseRequest,
    pause: bool,
) -> ApiResult<Json<PauseResponse>> {
    let scopes = validate_scopes(request.scopes)?;
    let product = repository::find_product_by_slug(&state.pool, slug)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let account = require_account(state, product.id, external_id).await?;
    let updated = repository::mutate_account_scopes(
        &state.pool,
        product.id,
        account.id,
        &scopes,
        pause,
        &admin_actor(state),
    )
    .await?;
    Ok(Json(PauseResponse {
        paused_scopes: updated,
    }))
}

async fn mutate_route_scopes(
    state: &AppState,
    route: &str,
    request: PauseRequest,
    pause: bool,
) -> ApiResult<Json<RoutePauseResponse>> {
    if !state
        .routes
        .routes()
        .iter()
        .any(|candidate| candidate.route == route)
    {
        return Err(ApiError::not_found());
    }
    let scopes = validate_scopes(request.scopes)?;
    let updated = repository::mutate_route_scopes(
        &state.pool,
        route,
        &scopes,
        pause,
        &format!("admin:{}", state.admin_key.kid),
    )
    .await?;
    Ok(Json(RoutePauseResponse {
        route: route.to_owned(),
        paused_scopes: updated,
    }))
}

pub(super) fn validate_external_id(external_id: &str) -> ApiResult<()> {
    if external_id.is_empty() || external_id.len() > 255 {
        return Err(ApiError::invalid_param(
            "account_id",
            "account_id must contain 1 to 255 bytes",
        ));
    }
    Ok(())
}

fn validate_reason(reason: &str) -> ApiResult<()> {
    if reason.trim().is_empty() || reason.len() > 1024 {
        return Err(ApiError::bad_request("reason must contain 1 to 1024 bytes"));
    }
    Ok(())
}

fn validate_product_slug(slug: &str) -> ApiResult<()> {
    let bytes = slug.as_bytes();
    let valid = matches!(bytes.first(), Some(b'a'..=b'z' | b'0'..=b'9'))
        && bytes.len() <= 63
        && bytes
            .iter()
            .all(|byte| matches!(byte, b'a'..=b'z' | b'0'..=b'9' | b'-'));
    if !valid {
        return Err(ApiError::bad_request(
            "slug must match ^[a-z0-9][a-z0-9-]{0,62}$",
        ));
    }
    Ok(())
}

/// Validates a product's credentials against the attested route that names its slug. The key id
/// comes only from that route, so a product no loaded route names could never authenticate.
fn validate_product_credentials(
    state: &AppState,
    slug: &str,
    public_key: &str,
    webhook_url: &str,
) -> ApiResult<()> {
    validate_product_slug(slug)?;
    VerificationKey::from_base64(String::new(), public_key).map_err(|_| {
        ApiError::bad_request("public_key must be standard base64 of a 32-byte ed25519 key")
    })?;
    state
        .routes
        .destination(slug)
        .ok_or_else(|| ApiError::bad_request("no loaded route names this product slug"))?;
    let local_stack = state.public_origin.to_string().starts_with("http://");
    validate_webhook_url(webhook_url, local_stack)
}

/// Requires an absolute `https` URL without credentials or fragment. `http` is accepted only
/// when the service's own public origin is `http`, which only local stacks use.
fn validate_webhook_url(webhook_url: &str, allow_http: bool) -> ApiResult<()> {
    const MESSAGE: &str = "webhook_url must be an absolute https URL without credentials";
    if webhook_url.len() > 2048 {
        return Err(ApiError::bad_request(MESSAGE));
    }
    let url = url::Url::parse(webhook_url).map_err(|_| ApiError::bad_request(MESSAGE))?;
    let scheme_allowed = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_allowed
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::bad_request(MESSAGE));
    }
    Ok(())
}

fn validate_scopes(scopes: Vec<String>) -> ApiResult<Vec<String>> {
    if scopes.is_empty() {
        return Err(ApiError::bad_request("at least one scope is required"));
    }
    let mut validated = scopes
        .into_iter()
        .map(|scope| {
            PauseScope::from_str(&scope)
                .map(|parsed| parsed.code().to_owned())
                .map_err(|_| ApiError::bad_request(format!("unknown pause scope `{scope}`")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validated.sort();
    validated.dedup();
    Ok(validated)
}

fn decode_nonce(value: &str) -> ApiResult<Vec<u8>> {
    if value.is_empty() || value.len() > 64 {
        return Err(ApiError::bad_request(
            "nonce must be 1 to 32 bytes of hexadecimal",
        ));
    }
    hex::decode(value).map_err(|_| ApiError::bad_request("nonce must be valid hexadecimal"))
}

fn product_response(product: Product) -> ProductResponse {
    ProductResponse {
        id: product.id,
        slug: product.slug,
        public_key: product.pubkey,
        webhook_url: product.webhook_url,
        paused_scopes: product.paused_scopes,
    }
}

fn admin_actor(state: &AppState) -> String {
    format!("admin:{}", state.admin_key.kid)
}

#[cfg(test)]
mod tests {
    use super::{validate_product_slug, validate_webhook_url};

    #[test]
    fn product_slugs_match_the_documented_pattern() {
        for slug in ["a", "0", "phala-cloud", "a-", &"a".repeat(63)] {
            assert!(validate_product_slug(slug).is_ok(), "{slug}");
        }
        for slug in ["", "-a", "A", "a_b", "a.b", "a b", &"a".repeat(64)] {
            assert!(validate_product_slug(slug).is_err(), "{slug}");
        }
    }

    #[test]
    fn webhook_urls_use_https_unless_the_service_origin_is_http() {
        assert!(validate_webhook_url("https://product.example/webhooks", false).is_ok());
        assert!(validate_webhook_url("http://product.example/webhooks", false).is_err());
        assert!(validate_webhook_url("http://product:8089/webhooks", true).is_ok());
        for url in [
            "product.example/webhooks",
            "ftp://product.example/webhooks",
            "https://user@product.example/webhooks",
            "https://product.example/webhooks#fragment",
        ] {
            assert!(validate_webhook_url(url, true).is_err(), "{url}");
        }
    }
}
