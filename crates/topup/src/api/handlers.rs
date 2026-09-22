//! Typed API handlers for the routes in architecture §12.

use std::str::FromStr;

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use topup_core::route::RouteFile;
use topup_core::screening::PauseScope;
use uuid::Uuid;

use crate::db::{Account, Product};

use super::AppState;
use super::attestation::AttestationError;
use super::error::{ApiError, ErrorResponse};
use super::models::{
    AccountResponse, AdminRefundResponse, AttestationQuery, AttestationResponse,
    DailyReportResponse, DepositAddressResponse, DepositListQuery, DepositLookupQuery,
    DepositResponse, DepositsResponse, LimitsResponse, NudgeResponse, PauseRequest, PauseResponse,
    PersistentSaltInputs, RecordRefundRequest, RefundRequest, RefundResponse,
    RegisterAccountRequest, RotateDepositAddressRequest, RoutePauseResponse,
};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts",
    params(("p" = String, Path, description = "Product slug")),
    request_body = RegisterAccountRequest,
    responses(
        (status = 200, body = AccountResponse),
        (status = 400, body = ErrorResponse),
        (status = 401, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "accounts"
)]
pub(crate) async fn register_account(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path(_product_slug): Path<String>,
    Json(request): Json<RegisterAccountRequest>,
) -> ApiResult<Json<AccountResponse>> {
    validate_external_id(&request.external_id)?;
    let account =
        repository::register_account(&state.pool, product.id, &request.external_id).await?;
    Ok(Json(account_response(account)))
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/accounts/{ext}/deposit-address",
    params(
        ("p" = String, Path, description = "Product slug"),
        ("ext" = String, Path, description = "Product-owned account identifier")
    ),
    responses(
        (status = 200, body = DepositAddressResponse),
        (status = 401, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 423, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "addresses"
)]
pub(crate) async fn get_deposit_address(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
) -> ApiResult<Json<DepositAddressResponse>> {
    deposit_address(&state, &product, &external_id, None).await
}

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts/{ext}/deposit-address",
    params(
        ("p" = String, Path, description = "Product slug"),
        ("ext" = String, Path, description = "Product-owned account identifier")
    ),
    responses(
        (status = 200, body = DepositAddressResponse),
        (status = 401, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 423, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "addresses"
)]
pub(crate) async fn create_deposit_address(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
) -> ApiResult<Json<DepositAddressResponse>> {
    deposit_address(&state, &product, &external_id, None).await
}

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts/{ext}/deposit-address/rotate",
    params(
        ("p" = String, Path, description = "Product slug"),
        ("ext" = String, Path, description = "Product-owned account identifier")
    ),
    request_body = RotateDepositAddressRequest,
    responses(
        (status = 200, body = DepositAddressResponse),
        (status = 401, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 409, body = ErrorResponse),
        (status = 423, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "addresses"
)]
pub(crate) async fn rotate_deposit_address(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
    Json(request): Json<RotateDepositAddressRequest>,
) -> ApiResult<Json<DepositAddressResponse>> {
    deposit_address(&state, &product, &external_id, Some(request.from_version)).await
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/accounts/{ext}/deposits",
    params(
        ("p" = String, Path),
        ("ext" = String, Path),
        DepositListQuery
    ),
    responses(
        (status = 200, body = DepositsResponse),
        (status = 400, body = ErrorResponse),
        (status = 404, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
pub(crate) async fn list_deposits(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
    Query(filters): Query<DepositListQuery>,
) -> ApiResult<Json<DepositsResponse>> {
    validate_state(filters.state.as_deref())?;
    let account = require_account(&state, product.id, &external_id).await?;
    Ok(Json(
        repository::list_account_deposits(&state.pool, product.id, account.id, &filters).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/deposits/{id}",
    params(("p" = String, Path), ("id" = Uuid, Path)),
    responses(
        (status = 200, body = DepositResponse),
        (status = 404, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
pub(crate) async fn get_deposit(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, deposit_id)): Path<(String, Uuid)>,
) -> ApiResult<Json<DepositResponse>> {
    let deposit = repository::get_product_deposit(&state.pool, product.id, deposit_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(deposit))
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/deposits",
    params(("p" = String, Path), DepositLookupQuery),
    responses(
        (status = 200, body = DepositsResponse),
        (status = 400, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
pub(crate) async fn lookup_deposits(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path(_product_slug): Path<String>,
    Query(filters): Query<DepositLookupQuery>,
) -> ApiResult<Json<DepositsResponse>> {
    Ok(Json(
        repository::lookup_product_deposits(&state.pool, product.id, &filters).await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/accounts/{ext}/limits",
    params(("p" = String, Path), ("ext" = String, Path)),
    responses((status = 200, body = LimitsResponse), (status = 404, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "accounts"
)]
pub(crate) async fn get_limits(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
) -> ApiResult<Json<LimitsResponse>> {
    let account = require_account(&state, product.id, &external_id).await?;
    let route = state.route_for_product(&product)?;
    let availability = crate::locks::exposure_availability(
        &state.pool,
        account.id,
        route.rate_lock.max_open_minor.account,
    )
    .await
    .map_err(|error| match error {
        crate::locks::RateLockError::Database(error) => ApiError::from(error),
        _ => ApiError::internal(),
    })?;
    Ok(Json(LimitsResponse {
        route: route.route.clone(),
        min_deposit_atomic: route.screening.min_deposit_atomic.value().to_string(),
        max_deposit_atomic: route.screening.max_deposit_atomic.value().to_string(),
        min_credit_minor: route.screening.min_credit_minor,
        account_open_minor: route.rate_lock.max_open_minor.account,
        product_open_minor: route.rate_lock.max_open_minor.product,
        global_open_minor: route.rate_lock.max_open_minor.global,
        remaining_account_minor: Some(availability.remaining_minor),
        reset_at: availability.reset_at,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts/{ext}/pause",
    params(("p" = String, Path), ("ext" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, body = PauseResponse), (status = 400, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "pauses"
)]
pub(crate) async fn pause_account(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &product, &external_id, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts/{ext}/resume",
    params(("p" = String, Path), ("ext" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, body = PauseResponse), (status = 400, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "pauses"
)]
pub(crate) async fn resume_account(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
    Json(request): Json<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &product, &external_id, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/products/{p}/deposits/{id}/refund-requests",
    params(("p" = String, Path), ("id" = Uuid, Path)),
    request_body = RefundRequest,
    responses(
        (status = 200, body = RefundResponse),
        (status = 501, body = ErrorResponse),
        (status = 404, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "refunds"
)]
pub(crate) async fn request_refund(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, deposit_id)): Path<(String, Uuid)>,
    Json(_request): Json<RefundRequest>,
) -> ApiResult<impl IntoResponse> {
    repository::get_product_deposit(&state.pool, product.id, deposit_id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Err::<StatusCode, _>(ApiError::not_implemented("C12"))
}

#[utoipa::path(
    get,
    path = "/v1/attestation",
    params(AttestationQuery),
    responses(
        (status = 200, body = AttestationResponse),
        (status = 400, body = ErrorResponse),
        (status = 501, body = ErrorResponse),
        (status = 503, body = ErrorResponse)
    ),
    tag = "attestation"
)]
pub(crate) async fn get_attestation(
    State(state): State<AppState>,
    Query(query): Query<AttestationQuery>,
) -> ApiResult<Json<AttestationResponse>> {
    let nonce = decode_nonce(&query.nonce)?;
    match state.attestor.attest(&nonce).await {
        Ok(response) => Ok(Json(response)),
        Err(AttestationError::NotConfigured) => Err(ApiError::not_implemented("C11")),
        Err(AttestationError::Unavailable) => {
            Err(ApiError::service_unavailable("attestation is unavailable"))
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/admin/routes/{r}/pause",
    params(("r" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, body = RoutePauseResponse), (status = 404, body = ErrorResponse)),
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
    responses((status = 200, body = RoutePauseResponse), (status = 404, body = ErrorResponse)),
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
    responses((status = 200, body = NudgeResponse), (status = 501, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn nudge_deposit(Path(_deposit_id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    Err::<StatusCode, _>(ApiError::not_implemented("C12"))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/approve",
    params(("id" = Uuid, Path)),
    responses((status = 200, body = AdminRefundResponse), (status = 501, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn approve_refund(Path(_refund_id): Path<Uuid>) -> ApiResult<impl IntoResponse> {
    Err::<StatusCode, _>(ApiError::not_implemented("C12"))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/record",
    params(("id" = Uuid, Path)),
    request_body = RecordRefundRequest,
    responses((status = 200, body = AdminRefundResponse), (status = 501, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn record_refund(
    Path(_refund_id): Path<Uuid>,
    Json(_request): Json<RecordRefundRequest>,
) -> ApiResult<impl IntoResponse> {
    Err::<StatusCode, _>(ApiError::not_implemented("C12"))
}

#[utoipa::path(
    get,
    path = "/v1/admin/report/daily",
    responses((status = 200, body = DailyReportResponse), (status = 501, body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn daily_report() -> ApiResult<impl IntoResponse> {
    Err::<StatusCode, _>(ApiError::not_implemented("C12"))
}

async fn deposit_address(
    state: &AppState,
    product: &Product,
    external_id: &str,
    rotate_from_version: Option<u64>,
) -> ApiResult<Json<DepositAddressResponse>> {
    let account = require_account(state, product.id, external_id).await?;
    let route = state.route_for_product(product)?;
    let route_scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
    if has_scope(&product.paused_scopes, "addresses")
        || has_scope(&account.paused_scopes, "addresses")
        || has_scope(&route_scopes, "addresses")
    {
        return Err(ApiError::paused("deposit addresses are paused"));
    }
    let address = if let Some(from_version) = rotate_from_version {
        repository::rotate_persistent_address(
            &state.pool,
            product.id,
            &account,
            &product.slug,
            route.chain.chain_id,
            route.chain.contracts.forwarder_factory,
            route.chain.contracts.implementation,
            from_version,
        )
        .await?
    } else {
        repository::get_or_create_persistent_address(
            &state.pool,
            product.id,
            &account,
            &product.slug,
            route.chain.chain_id,
            route.chain.contracts.forwarder_factory,
            route.chain.contracts.implementation,
        )
        .await?
    };
    Ok(Json(address_response(route, product, &account, address)))
}

fn has_scope(scopes: &[String], expected: &str) -> bool {
    scopes.iter().any(|scope| scope == expected)
}

fn address_response(
    route: &RouteFile,
    product: &Product,
    account: &Account,
    address: crate::db::Address,
) -> DepositAddressResponse {
    DepositAddressResponse {
        chain_id: address.chain_id,
        route: route.route.clone(),
        address: format!("{:#x}", address.address),
        salt: format!("{:#x}", address.salt),
        salt_inputs: PersistentSaltInputs {
            product_slug: product.slug.clone(),
            external_id: account.external_id.clone(),
            version: address.version,
        },
    }
}

async fn require_account(
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
    product: &Product,
    external_id: &str,
    request: PauseRequest,
    pause: bool,
) -> ApiResult<Json<PauseResponse>> {
    let scopes = validate_scopes(request.scopes)?;
    let account = require_account(state, product.id, external_id).await?;
    let updated = repository::mutate_account_scopes(
        &state.pool,
        product.id,
        account.id,
        &scopes,
        pause,
        &format!("product:{}", product.id),
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

fn validate_external_id(external_id: &str) -> ApiResult<()> {
    if external_id.is_empty() || external_id.len() > 255 {
        return Err(ApiError::bad_request(
            "external_id must contain 1 to 255 bytes",
        ));
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

fn validate_state(state: Option<&str>) -> ApiResult<()> {
    if let Some(state) = state
        && !matches!(
            state,
            "detected" | "confirmed" | "cleared" | "credited" | "swept" | "rejected"
        )
    {
        return Err(ApiError::bad_request("unknown deposit state"));
    }
    Ok(())
}

fn decode_nonce(value: &str) -> ApiResult<Vec<u8>> {
    if value.is_empty() || value.len() > 64 {
        return Err(ApiError::bad_request(
            "nonce must be 1 to 32 bytes of hexadecimal",
        ));
    }
    hex::decode(value).map_err(|_| ApiError::bad_request("nonce must be valid hexadecimal"))
}

fn account_response(account: Account) -> AccountResponse {
    AccountResponse {
        id: account.id,
        external_id: account.external_id,
        paused_scopes: account.paused_scopes,
    }
}
