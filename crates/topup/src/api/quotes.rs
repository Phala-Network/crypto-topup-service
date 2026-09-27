//! Quotes (`/v1/quotes`) and the product configuration (`/v1/config`).

use axum::Json;
use axum::extract::{Extension, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{IntoResponse as _, Response};
use serde::Deserialize;
use topup_core::money::MinorAmount;
use topup_core::route::{PricingMode, RouteFile};

use crate::db::{Account, Product};
use crate::ids;
use crate::locks::{self, RateLock, RateLockError, RateLockStatus};

use super::AppState;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, idempotency_key};
use super::handlers::{ensure_account, validate_external_id};
use super::models::{ClientQuote, Config, ConfigAsset, CreateQuoteRequest, Quote, QuoteView};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

/// Typical Ethereum delay from inclusion to the `finalized` tag (architecture §12).
const TYPICAL_FINALITY_SECONDS: u64 = 900;

#[utoipa::path(
    get,
    path = "/v1/config",
    responses(
        (status = 200, description = "OK", body = Config),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "config"
)]
/// The calling product's assets, limits, and quote terms, from its attested routes.
pub(crate) async fn get_config(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
) -> ApiResult<Json<Config>> {
    let routes = product_routes(&state, &product).collect::<Vec<_>>();
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
            typical_finality_seconds: TYPICAL_FINALITY_SECONDS,
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
            description = "Up to 255 characters; a repeat with the same parameters returns the \
                           same quote, and with other parameters is `409 idempotency_error`."
        )
    ),
    request_body = CreateQuoteRequest,
    responses(
        (status = 200, description = "OK", body = Quote),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (
            status = 409,
            description = "`exposure_cap_exceeded`, `paused`, `chain_frozen`, \
                           `signature_replayed`, or `idempotency_error`",
            body = ErrorResponse
        ),
        (status = 429, description = "Too Many Requests", body = ErrorResponse),
        (status = 503, description = "Service Unavailable", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "quotes"
)]
/// Quotes `amount` cents payable in `asset` on `chain_id`: a locked price, the exact token amount,
/// and a single-use address, valid until `expires_at`.
pub(crate) async fn create_quote(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateQuoteRequest>,
) -> ApiResult<Json<Quote>> {
    let key = idempotency_key(&headers)?;
    validate_external_id(&request.account_id)?;
    if request.currency != "usd" {
        return Err(ApiError::invalid_param("currency", "currency must be usd"));
    }
    let route = product_routes(&state, &product)
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
    let account = ensure_account(&state, product.id, &request.account_id).await?;
    // A repeat creates nothing, so a `quotes` pause does not hide a quote the product already
    // showed.
    if let Some(key) = key.as_deref()
        && let Some(existing) = locks::find_by_idempotency_key(&state.pool, product.id, key)
            .await
            .map_err(map_error)?
    {
        let lock = locks::replay(existing, &account, route, credit).map_err(map_error)?;
        return respond_with_client_secret(&state, &product, lock).await;
    }
    if crate::reconciler::chain_is_blocked(&state.pool, route.chain.chain_id).await? {
        return Err(ApiError::chain_frozen());
    }
    let route_scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
    if has_quotes_pause(&product, &account, &route_scopes) {
        return Err(ApiError::paused("quotes are paused"));
    }
    let lock = locks::create(
        &state.pool,
        &state.rate_lock_quotes,
        &product,
        &account,
        route,
        key.as_deref(),
        credit,
    )
    .await
    .map_err(map_error)?;
    respond_with_client_secret(&state, &product, lock).await
}

/// Query of `GET /v1/quotes/{id}`.
#[derive(Debug, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub(crate) struct GetQuoteQuery {
    /// The quote's `client_secret`, to read its public view without a signature. Send the request
    /// without `Signature` headers; the response then allows any origin.
    client_secret: Option<String>,
}

#[utoipa::path(
    get,
    path = "/v1/quotes/{id}",
    params(("id" = String, Path, description = "Quote id, `qt_…`"), GetQuoteQuery),
    responses(
        (
            status = 200,
            description = "OK: a `Quote` to a signed request, a `ClientQuote` to a request by \
                           `client_secret`",
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
    security(("http_message_signature" = []), ()),
    tag = "quotes"
)]
/// One quote, for example to resume a checkout page. The payer's browser can read the quote's
/// public view with its `client_secret` instead of a signature, as Stripe.js reads a PaymentIntent.
pub(crate) async fn get_quote(
    State(state): State<AppState>,
    product: Option<Extension<Product>>,
    Path(id): Path<String>,
    Query(query): Query<GetQuoteQuery>,
) -> Response {
    let Some(Extension(product)) = product else {
        let mut response = match client_quote(&state, &id, query.client_secret.as_deref()).await {
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
        let quote = ids::parse(ids::QUOTE, &id).ok_or_else(ApiError::not_found)?;
        let lock = locks::get(&state.pool, product.id, quote)
            .await
            .map_err(map_error)?
            .ok_or_else(ApiError::not_found)?;
        quote_object(&state, &product, lock).await
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
    let payment = super::pending::quote_payment(state, route, &lock).await?;
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
        id: locks::quote_id(lock.address_id),
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
    params(("id" = String, Path, description = "Quote id, `qt_…`")),
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
    security(("http_message_signature" = [])),
    tag = "quotes"
)]
/// Cancels an open, unpaid quote; a canceled quote is returned unchanged. Later payments to its
/// address are credited at spot.
pub(crate) async fn cancel_quote(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path(id): Path<String>,
) -> ApiResult<Json<Quote>> {
    let quote = ids::parse(ids::QUOTE, &id).ok_or_else(ApiError::not_found)?;
    let lock = locks::cancel(&state.pool, &product, quote)
        .await
        .map_err(map_error)?;
    respond(&state, &product, lock).await
}

fn product_routes<'a>(
    state: &'a AppState,
    product: &'a Product,
) -> impl Iterator<Item = &'a RouteFile> {
    state
        .routes
        .current()
        .filter(move |route| route.destination.product == product.slug)
}

async fn respond(state: &AppState, product: &Product, lock: RateLock) -> ApiResult<Json<Quote>> {
    quote_object(state, product, lock).await.map(Json)
}

/// Responds to `POST /v1/quotes` with a newly issued client secret.
async fn respond_with_client_secret(
    state: &AppState,
    product: &Product,
    lock: RateLock,
) -> ApiResult<Json<Quote>> {
    let client_secret = locks::issue_client_secret(&state.pool, lock.address_id)
        .await
        .map_err(map_error)?;
    let mut quote = quote_object(state, product, lock).await?;
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

async fn quote_object(state: &AppState, product: &Product, lock: RateLock) -> ApiResult<Quote> {
    // The quote's own route version may be retired; asset and tolerance come from the route's
    // current version, which keeps the chain and asset.
    let route = product_routes(state, product)
        .find(|route| route.route == lock.route)
        .ok_or_else(|| {
            tracing::error!(route = %lock.route, "quote route is not loaded");
            ApiError::internal()
        })?;
    let payment = super::pending::quote_payment(state, route, &lock).await?;
    let payment_uri = payment_uri(route, &lock);
    Ok(Quote {
        id: locks::quote_id(lock.address_id),
        object: "quote".to_owned(),
        account_id: lock.account_external_id,
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
            .map(|deposit| ids::format(ids::DEPOSIT, deposit)),
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
fn decimal(scaled: u64) -> String {
    let digits = format!("{scaled:09}");
    let (integer, fraction) = digits.split_at(digits.len().saturating_sub(8));
    format!("{integer}.{fraction}")
}

fn has_quotes_pause(product: &Product, account: &Account, route_scopes: &[String]) -> bool {
    product.paused_scopes.iter().any(|scope| scope == "quotes")
        || account.paused_scopes.iter().any(|scope| scope == "quotes")
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
        RateLockError::IdempotencyMismatch => ApiError::idempotency_key_reused(),
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
