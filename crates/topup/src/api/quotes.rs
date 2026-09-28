//! Quotes (`/v1/quotes`) and the product configuration (`/v1/config`).

use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse as _, Response};
use sqlx::PgPool;
use topup_core::money::MinorAmount;
use topup_core::route::{PricingMode, RouteFile};
use uuid::Uuid;

use crate::db::{Account, Customer};
use crate::ids;
use crate::locks::{self, RateLock, RateLockError, RateLockStatus};
use crate::routes::RouteSet;
use crate::tenancy::{Permission, Scope};

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, expansions, query_pairs};
use super::handlers::{ensure_customer, validate_external_id};
use super::models::{
    ClientQuote, Config, ConfigAsset, CreateQuoteRequest, ExpandableDeposit, Quote, QuoteView,
};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

use topup_core::route::{Confirmations, TYPICAL_FINALIZED_SECONDS};

#[utoipa::path(
    get,
    path = "/v1/config",
    responses(
        (status = 200, description = "OK", body = Config),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "config"
)]
/// The assets, limits, and quote terms of the attested routes in the credential's mode.
pub(crate) async fn get_config(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
) -> ApiResult<Json<Config>> {
    merchant
        .require(&state.pool, Permission::AccountRead)
        .await?;
    let routes = state
        .routes
        .current_in(merchant.scope.livemode())
        .collect::<Vec<_>>();
    let max_open_amount_per_account = routes
        .iter()
        .map(|route| route.rate_lock.max_open_minor.account)
        .min()
        .unwrap_or_default();
    let assets = routes
        .iter()
        .map(|route| ConfigAsset {
            chain_id: route.chain.chain_id,
            asset: route.asset.symbol.clone(),
            contract: format!("{:#x}", route.asset.contract),
            decimals: route.asset.decimals,
            pricing: match route.pricing.mode {
                PricingMode::Spot => "spot",
                PricingMode::Stablecoin => "stablecoin",
            }
            .to_owned(),
            min_amount: route.screening.min_credit_minor,
            max_deposit_atomic: route.screening.max_deposit_atomic.value().to_string(),
            min_refund_atomic: route.asset.min_refund_atomic.value().to_string(),
            quote_ttl_seconds: route.rate_lock.window_s,
            quote_spread_bps: route.rate_lock.spread_bps.value(),
            quote_tolerance_bps: route.rate_lock.lock_tolerance_bps.value(),
            confirmations: match route.chain.confirmations {
                Confirmations::Depth(depth) => depth.to_string(),
                Confirmations::Safe => "safe".to_owned(),
                Confirmations::Finalized => "finalized".to_owned(),
            },
            typical_credit_seconds: route.chain.confirmations.typical_credit_seconds(),
            typical_finality_seconds: TYPICAL_FINALIZED_SECONDS,
        })
        .collect();
    Ok(Json(Config {
        object: "config".to_owned(),
        currency: "usd".to_owned(),
        max_open_amount_per_account,
        assets,
    }))
}

#[utoipa::path(
    post,
    path = "/v1/quotes",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateQuoteRequest,
    responses(
        (status = 200, description = "OK", body = Quote),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (
            status = 409,
            description = "`exposure_cap_exceeded`, `paused`, `chain_frozen`, or \
                           `idempotency_key_in_use`",
            body = ErrorResponse
        ),
        (status = 429, description = "Too Many Requests", body = ErrorResponse),
        (status = 503, description = "Service Unavailable", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "quotes"
)]
/// Quotes `amount` cents payable in `asset` on `chain_id`: a locked price, the exact token amount,
/// and a single-use address, valid until `expires_at`.
pub(crate) async fn create_quote(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateQuoteRequest>,
) -> ApiResult<Json<Quote>> {
    merchant
        .require(&state.pool, Permission::QuotesWrite)
        .await?;
    validate_external_id(&request.account_id)?;
    if request.currency != "usd" {
        return Err(ApiError::invalid_param("currency", "currency must be usd"));
    }
    let route = state
        .routes
        .current_in(merchant.scope.livemode())
        .find(|route| {
            route.chain.chain_id == request.chain_id && route.asset.symbol == request.asset
        })
        .ok_or_else(|| {
            ApiError::invalid_param("asset", "no payable asset matches chain_id and asset")
        })?;
    if request.amount < route.screening.min_credit_minor.max(1) {
        return Err(ApiError::amount_too_small(
            "amount",
            format!(
                "amount must be at least {}",
                route.screening.min_credit_minor.max(1)
            ),
        ));
    }
    let credit = MinorAmount::new(request.amount);
    let customer = ensure_customer(&state, merchant.scope, &request.account_id).await?;
    if crate::reconciler::chain_is_blocked(&state.pool, route.chain.chain_id).await? {
        return Err(ApiError::chain_frozen());
    }
    let route_scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
    if has_quotes_pause(&merchant.account, &customer, &route_scopes) {
        return Err(ApiError::paused("quotes are paused"));
    }
    let lock = locks::create(
        &state.pool,
        &state.rate_lock_quotes,
        &merchant.account,
        &customer,
        route,
        credit,
    )
    .await
    .map_err(map_error)?;
    respond_with_client_secret(&state, lock).await
}

#[utoipa::path(
    get,
    path = "/v1/quotes/{id}",
    params(
        ("id" = String, Path, description = "Quote id, `qt_…`"),
        ("expand[]" = Option<Vec<String>>, Query, description = "`deposit`; API key requests only"),
        (
            "client_secret" = Option<String>, Query,
            description = "The quote's `client_secret`, to read its public view without an \
                           API key. Send the request without `Authorization`; the response then \
                           allows any origin."
        )
    ),
    responses(
        (
            status = 200,
            description = "OK: a `Quote` to a request with an API key, a `ClientQuote` to a \
                           request by `client_secret`",
            body = QuoteView
        ),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (
            status = 404,
            description = "Not Found, also for a `client_secret` that is not this quote's",
            body = ErrorResponse
        ),
        (status = 429, description = "Too Many Requests: reads by `client_secret`", body = ErrorResponse)
    ),
    security(("api_key" = []), ()),
    tag = "quotes"
)]
/// One quote, for example to resume a checkout page. The payer's browser can read the quote's
/// public view with its `client_secret` instead of an API key, as Stripe.js reads a PaymentIntent.
pub(crate) async fn get_quote(
    State(state): State<AppState>,
    merchant: Option<Extension<Merchant>>,
    ApiPath(id): ApiPath<String>,
    RawQuery(query): RawQuery,
) -> Response {
    let pairs = query_pairs(query.as_deref());
    let Some(Extension(merchant)) = merchant else {
        let client_secret = pairs
            .iter()
            .find(|(name, _)| name == "client_secret")
            .map(|(_, value)| value.as_str());
        let mut response = match client_quote(&state, &id, client_secret).await {
            Ok(quote) => Json(QuoteView::Client(quote)).into_response(),
            Err(error) => error.into_response(),
        };
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_ORIGIN,
            HeaderValue::from_static("*"),
        );
        return response;
    };
    let quote = async {
        merchant
            .require(&state.pool, Permission::QuotesRead)
            .await?;
        let expand = expansions(&pairs, &["deposit"])?;
        let quote = ids::parse(ids::QUOTE, &id).ok_or_else(ApiError::not_found)?;
        let lock = locks::get(&state.pool, merchant.scope, quote)
            .await
            .map_err(map_error)?
            .ok_or_else(ApiError::not_found)?;
        let consumed_by = lock.consumed_by;
        let mut quote = quote_object(&state.pool, &state.routes, lock).await?;
        if expand.contains(&"deposit")
            && let Some(deposit) = consumed_by
        {
            let deposit =
                super::deposits::find_deposit(&state.pool, &state.routes, merchant.scope, deposit)
                    .await?
                    .ok_or_else(ApiError::internal)?;
            quote.deposit = Some(ExpandableDeposit::Object(Box::new(deposit)));
        }
        Ok::<_, ApiError>(quote)
    };
    match quote.await {
        Ok(quote) => Json(QuoteView::Quote(quote)).into_response(),
        Err(error) => error.into_response(),
    }
}

/// The public view of the quote `id` whose client secret is `client_secret`.
async fn client_quote(
    state: &AppState,
    id: &str,
    client_secret: Option<&str>,
) -> ApiResult<ClientQuote> {
    let quote = ids::parse(ids::QUOTE, id).ok_or_else(ApiError::not_found)?;
    let client_secret = client_secret
        .filter(|secret| {
            secret
                .strip_prefix(id)
                .is_some_and(|rest| rest.starts_with("_secret_"))
        })
        .ok_or_else(ApiError::not_found)?;
    if !state.client_reads.allow(quote) {
        return Err(ApiError::rate_limited());
    }
    let lock = locks::get_by_client_secret(&state.pool, client_secret)
        .await
        .map_err(map_error)?
        .ok_or_else(ApiError::not_found)?;
    let route = state
        .routes
        .current()
        .find(|route| route.route == lock.route)
        .ok_or_else(|| {
            tracing::error!(route = %lock.route, "quote route is not loaded");
            ApiError::internal()
        })?;
    let payment = super::pending::quote_payment(&state.pool, route, &lock).await?;
    let (payment_status, confirmations) = match payment {
        None => ("none", None),
        Some(payment) if payment.status == "seen" => ("seen", payment.confirmations),
        Some(payment) => {
            let deposit =
                ids::parse(ids::DEPOSIT, &payment.deposit).ok_or_else(ApiError::internal)?;
            let deposit_state =
                sqlx::query_scalar::<_, String>("SELECT state FROM deposits WHERE id = $1")
                    .bind(deposit)
                    .fetch_one(&state.pool)
                    .await?;
            let status = match deposit_state.as_str() {
                "credited" | "swept" => "credited",
                "rejected" => "rejected",
                _ => "confirming",
            };
            (status, None)
        }
    };
    Ok(ClientQuote {
        id: locks::quote_id(lock.id),
        object: "quote".to_owned(),
        status: status(lock.status).to_owned(),
        amount: lock.credit_minor.value(),
        currency: "usd".to_owned(),
        asset: route.asset.symbol.clone(),
        decimals: route.asset.decimals,
        chain_id: lock.chain_id,
        amount_atomic: lock.amount_atomic.value().to_string(),
        address: format!("{:#x}", lock.address),
        payment_uri: payment_uri(route, &lock),
        expires_at: lock.expires_at.timestamp(),
        payment_status: payment_status.to_owned(),
        confirmations,
    })
}

#[utoipa::path(
    post,
    path = "/v1/quotes/{id}/cancel",
    params(
        ("id" = String, Path, description = "Quote id, `qt_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    responses(
        (status = 200, description = "OK", body = Quote),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (
            status = 409,
            description = "`quote_payment_received`: the address already received a payment; \
                           `quote_window_closed`: past `expires_at`; `quote_unexpected_state`: \
                           complete or expired",
            body = ErrorResponse
        )
    ),
    security(("api_key" = [])),
    tag = "quotes"
)]
/// Cancels an open, unpaid quote; a canceled quote is returned unchanged. Later payments to its
/// address are credited at spot.
pub(crate) async fn cancel_quote(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<Quote>> {
    merchant
        .require(&state.pool, Permission::QuotesWrite)
        .await?;
    let quote = ids::parse(ids::QUOTE, &id).ok_or_else(ApiError::not_found)?;
    let lock = locks::cancel(&state.pool, merchant.scope, &merchant.actor(), quote)
        .await
        .map_err(map_error)?;
    respond(&state, lock).await
}

async fn respond(state: &AppState, lock: RateLock) -> ApiResult<Json<Quote>> {
    quote_object(&state.pool, &state.routes, lock)
        .await
        .map(Json)
}

/// Responds to `POST /v1/quotes` with a newly issued client secret.
async fn respond_with_client_secret(state: &AppState, lock: RateLock) -> ApiResult<Json<Quote>> {
    let client_secret = locks::issue_client_secret(&state.pool, lock.id)
        .await
        .map_err(map_error)?;
    let mut quote = quote_object(&state.pool, &state.routes, lock).await?;
    quote.client_secret = Some(client_secret);
    Ok(Json(quote))
}

fn payment_uri(route: &RouteFile, lock: &RateLock) -> String {
    format!(
        "ethereum:{:#x}@{}/transfer?address={:#x}&uint256={}",
        route.asset.contract,
        lock.chain_id,
        lock.address,
        lock.amount_atomic.value()
    )
}

/// Returns the scope's quote `id`.
pub(crate) async fn find_quote(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    id: Uuid,
) -> ApiResult<Option<Quote>> {
    match locks::get(pool, scope, id).await.map_err(map_error)? {
        Some(lock) => quote_object(pool, routes, lock).await.map(Some),
        None => Ok(None),
    }
}

/// The API representation of a quote.
pub(crate) async fn quote_object(
    pool: &PgPool,
    routes: &RouteSet,
    lock: RateLock,
) -> ApiResult<Quote> {
    // The quote's own route version may be retired; asset and tolerance come from the route's
    // current version, which keeps the chain and asset.
    let route = routes
        .current()
        .find(|route| route.route == lock.route)
        .ok_or_else(|| {
            tracing::error!(route = %lock.route, "quote route is not loaded");
            ApiError::internal()
        })?;
    let payment = super::pending::quote_payment(pool, route, &lock).await?;
    let payment_uri = payment_uri(route, &lock);
    Ok(Quote {
        id: locks::quote_id(lock.id),
        object: "quote".to_owned(),
        account_id: lock.client_reference_id,
        amount: lock.credit_minor.value(),
        currency: "usd".to_owned(),
        chain_id: lock.chain_id,
        asset: route.asset.symbol.clone(),
        amount_atomic: lock.amount_atomic.value().to_string(),
        exchange_rate: decimal(lock.price.value()),
        address: format!("{:#x}", lock.address),
        payment_uri,
        status: status(lock.status).to_owned(),
        expires_at: lock.expires_at.timestamp(),
        created: lock.created_at.timestamp(),
        payment,
        deposit: lock
            .consumed_by
            .map(|deposit| ExpandableDeposit::Id(ids::format(ids::DEPOSIT, deposit))),
        client_secret: None,
    })
}

const fn status(status: RateLockStatus) -> &'static str {
    match status {
        RateLockStatus::Open => "open",
        RateLockStatus::Consumed => "complete",
        RateLockStatus::Expired => "expired",
        RateLockStatus::Cancelled => "canceled",
    }
}

/// An eight-decimal scaled price as an exact decimal string, such as `0.24875621`.
pub(super) fn decimal(scaled: u64) -> String {
    let digits = format!("{scaled:09}");
    let (integer, fraction) = digits.split_at(digits.len().saturating_sub(8));
    format!("{integer}.{fraction}")
}

fn has_quotes_pause(account: &Account, customer: &Customer, route_scopes: &[String]) -> bool {
    account.paused_scopes.iter().any(|scope| scope == "quotes")
        || customer.paused_scopes.iter().any(|scope| scope == "quotes")
        || route_scopes.iter().any(|scope| scope == "quotes")
}

fn map_error(error: RateLockError) -> ApiError {
    match error {
        RateLockError::InvalidInput(message) => ApiError::bad_request(message),
        RateLockError::AmountTooSmall(message) => ApiError::amount_too_small("amount", message),
        RateLockError::AmountTooLarge(message) => ApiError::amount_too_large("amount", message),
        RateLockError::PricingUnavailable => {
            ApiError::service_unavailable("validated pricing is unavailable")
        }
        RateLockError::RateLimited => ApiError::rate_limited(),
        error @ RateLockError::ExposureCap { .. } => ApiError::exposure_cap(error.to_string()),
        RateLockError::NotFound => ApiError::not_found(),
        RateLockError::NotOpen(current) => ApiError::quote_unexpected_state(status(current)),
        RateLockError::WindowClosed => ApiError::quote_window_closed(),
        RateLockError::PendingPayment => ApiError::quote_payment_received(),
        RateLockError::Arithmetic
        | RateLockError::EntropyUnavailable
        | RateLockError::DatabaseInvariant => ApiError::internal(),
        RateLockError::Database(error) => ApiError::from(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_prices_render_exactly() {
        assert_eq!(decimal(24_875_621), "0.24875621");
        assert_eq!(decimal(100_000_000), "1.00000000");
        assert_eq!(decimal(1_234_500_000_001), "12345.00000001");
        assert_eq!(decimal(0), "0.00000000");
    }
}
