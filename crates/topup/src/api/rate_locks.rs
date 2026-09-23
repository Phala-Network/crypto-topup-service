//! Product rate-lock HTTP handlers.

use std::str::FromStr;

use alloy_primitives::U256;
use axum::Json;
use axum::extract::{Extension, Path, State};
use chrono::Utc;
use topup_core::money::{AtomicAmount, MinorAmount};

use crate::db::{Account, Product};
use crate::locks::{self, RateLockError, RequestedAmount};

use super::AppState;
use super::error::{ApiError, ErrorResponse};
use super::models::{
    CancelRateLockResponse, CreateRateLockRequest, RateLockResponse, RateLockSaltInputs,
};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    post,
    path = "/v1/products/{p}/accounts/{ext}/rate-locks",
    params(("p" = String, Path), ("ext" = String, Path)),
    request_body = CreateRateLockRequest,
    responses(
        (status = 200, body = RateLockResponse),
        (status = 400, body = ErrorResponse),
        (status = 404, body = ErrorResponse),
        (status = 409, body = ErrorResponse),
        (status = 423, body = ErrorResponse),
        (status = 429, body = ErrorResponse),
        (status = 503, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "rate-locks"
)]
pub(crate) async fn create_rate_lock(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
    Json(request): Json<CreateRateLockRequest>,
) -> ApiResult<Json<RateLockResponse>> {
    let account = require_account(&state, product.id, &external_id).await?;
    let route = state.route_for_product(&product)?;
    if crate::reconciler::chain_is_blocked(&state.pool, route.chain.chain_id).await? {
        return Err(ApiError::chain_frozen());
    }
    let requested = parse_requested_amount(&request)?;
    // A replay creates nothing, so a `quotes` pause does not hide a lock the product already
    // showed; it answers exactly like `GET`, including the idempotency mismatch check.
    if let Some(lock) = locks::find_replay(
        &state.pool,
        product.id,
        account.id,
        &request.product_lock_ref,
        requested,
    )
    .await
    .map_err(map_error)?
    {
        return Ok(Json(response(route, &product, &account, lock)));
    }
    let route_scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
    if has_quotes_pause(&product, &account, &route_scopes) {
        return Err(ApiError::paused("rate-lock quotes are paused"));
    }
    let lock = locks::create(
        &state.pool,
        &state.rate_lock_quotes,
        &product,
        &account,
        route,
        &request.product_lock_ref,
        requested,
    )
    .await
    .map_err(map_error)?;
    Ok(Json(response(route, &product, &account, lock)))
}

#[utoipa::path(
    get,
    path = "/v1/products/{p}/accounts/{ext}/rate-locks/{ref}",
    params(("p" = String, Path), ("ext" = String, Path), ("ref" = String, Path)),
    responses(
        (status = 200, body = RateLockResponse),
        (status = 404, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "rate-locks"
)]
pub(crate) async fn get_rate_lock(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id, lock_ref)): Path<(String, String, String)>,
) -> ApiResult<Json<RateLockResponse>> {
    let account = require_account(&state, product.id, &external_id).await?;
    let route = state.route_for_product(&product)?;
    let lock = locks::get(&state.pool, product.id, account.id, &lock_ref)
        .await
        .map_err(map_error)?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(response(route, &product, &account, lock)))
}

#[utoipa::path(
    delete,
    path = "/v1/products/{p}/accounts/{ext}/rate-locks/{ref}",
    params(("p" = String, Path), ("ext" = String, Path), ("ref" = String, Path)),
    responses(
        (status = 200, body = CancelRateLockResponse),
        (status = 404, body = ErrorResponse),
        (status = 409, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "rate-locks"
)]
pub(crate) async fn cancel_rate_lock(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id, lock_ref)): Path<(String, String, String)>,
) -> ApiResult<Json<CancelRateLockResponse>> {
    let account = require_account(&state, product.id, &external_id).await?;
    let lock = locks::cancel(&state.pool, &product, &account, &lock_ref)
        .await
        .map_err(map_error)?;
    Ok(Json(CancelRateLockResponse {
        product_lock_ref: lock.lock_ref,
        status: lock.status.code().to_owned(),
    }))
}

fn parse_requested_amount(request: &CreateRateLockRequest) -> ApiResult<RequestedAmount> {
    match (&request.amount_minor, &request.amount_atomic) {
        (Some(minor), None) => parse_decimal_u64(minor)
            .map(MinorAmount::new)
            .map(RequestedAmount::Minor)
            .map_err(|_| ApiError::bad_request("amount_minor must be an unsigned 64-bit integer")),
        (None, Some(atomic)) => parse_decimal_u256(atomic)
            .map(AtomicAmount::new)
            .map(RequestedAmount::Atomic)
            .map_err(|_| {
                ApiError::bad_request("amount_atomic must be an unsigned 256-bit integer")
            }),
        _ => Err(ApiError::bad_request(
            "exactly one of amount_minor or amount_atomic is required",
        )),
    }
}

fn parse_decimal_u64(value: &str) -> Result<u64, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    value.parse().map_err(|_| ())
}

fn parse_decimal_u256(value: &str) -> Result<U256, ()> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(());
    }
    U256::from_str(value).map_err(|_| ())
}

fn response(
    route: &topup_core::route::RouteFile,
    product: &Product,
    account: &Account,
    lock: locks::RateLock,
) -> RateLockResponse {
    let now = Utc::now();
    RateLockResponse {
        address: format!("{:#x}", lock.address),
        amount_atomic: lock.amount_atomic.value().to_string(),
        price_scaled: lock.price.value().to_string(),
        credit_minor: lock.credit_minor.value().to_string(),
        expires_at: lock.expires_at,
        status: lock.status.code().to_owned(),
        remaining_seconds: lock.remaining_seconds(now),
        eip681_uri: format!(
            "ethereum:{:#x}@{}/transfer?address={:#x}&uint256={}",
            route.asset.contract,
            lock.chain_id,
            lock.address,
            lock.amount_atomic.value()
        ),
        salt_inputs: RateLockSaltInputs {
            product_slug: product.slug.clone(),
            external_id: account.external_id.clone(),
            lock_ref: lock.lock_ref,
        },
    }
}

fn has_quotes_pause(product: &Product, account: &Account, route_scopes: &[String]) -> bool {
    product.paused_scopes.iter().any(|scope| scope == "quotes")
        || account.paused_scopes.iter().any(|scope| scope == "quotes")
        || route_scopes.iter().any(|scope| scope == "quotes")
}

async fn require_account(
    state: &AppState,
    product_id: uuid::Uuid,
    external_id: &str,
) -> ApiResult<Account> {
    repository::find_account(&state.pool, product_id, external_id)
        .await?
        .ok_or_else(ApiError::not_found)
}

fn map_error(error: RateLockError) -> ApiError {
    match error {
        RateLockError::InvalidInput(message) => ApiError::bad_request(message),
        RateLockError::Disabled => ApiError::paused("rate-lock quotes are disabled"),
        RateLockError::PricingUnavailable => {
            ApiError::service_unavailable("validated pricing is unavailable")
        }
        RateLockError::RateLimited => ApiError::rate_limited(),
        RateLockError::ExposureCap(scope) => ApiError::exposure_cap(scope),
        RateLockError::NotFound => ApiError::not_found(),
        RateLockError::NotOpen => ApiError::conflict("rate lock is not open"),
        RateLockError::PendingPayment => ApiError::pending_payment(),
        RateLockError::IdempotencyMismatch => ApiError::idempotency_mismatch(),
        RateLockError::Arithmetic | RateLockError::DatabaseInvariant => ApiError::internal(),
        RateLockError::Database(error) => ApiError::from(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn amount_parsers_require_decimal_digits() {
        assert_eq!(parse_decimal_u64("0010"), Ok(10));
        assert_eq!(parse_decimal_u256("0010"), Ok(U256::from(10_u64)));
        for invalid in ["", "+1", "-1", "0x10", "1_000", " 1"] {
            assert_eq!(parse_decimal_u64(invalid), Err(()));
            assert_eq!(parse_decimal_u256(invalid), Err(()));
        }
    }
}
