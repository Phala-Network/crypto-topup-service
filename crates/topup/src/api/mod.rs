//! Axum API: merchant routes authenticated by API keys, the admin routes by RFC 9421
//! signatures, and the generated OpenAPI.

mod account;
mod attestation;
mod auth;
mod client_limit;
mod deposit_addresses;
mod deposits;
pub(crate) mod error;
mod events;
mod examples;
mod extract;
mod handlers;
mod idempotency;
mod keys;
pub(crate) mod metadata;
pub mod models;
mod openapi;
mod pagination;
mod pending;
mod quotes;
mod rate_limit;
mod repository;
mod restore;
mod sweeps;
mod treasuries;
mod webhook_endpoints;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::locks::QuoteProvider;
use crate::refunds::DestinationScreener;
use crate::routes::RouteSet;
use crate::tenancy::Scope;
use crate::treasuries::ContractSignatures;
use axum::extract::{Extension, Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use topup_core::route::RouteFile;

use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use attestation::{
    AttestationError, AttestationEvidence, AttestationFuture, AttestationRequest, Attestor,
};
pub use auth::VerificationKey;
pub use client_limit::ClientReadLimiter;
pub(crate) use keys::api_key_object;
pub use rate_limit::{ApiRateLimiter, RateLimits};
pub use topup_adapters::http_signature::PublicOrigin;
pub(crate) use treasuries::treasury_object;

/// Shared state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Application-role PostgreSQL connection pool.
    pub pool: PgPool,
    /// Attested route configurations.
    pub routes: Arc<RouteSet>,
    /// Separately configured administrative verification key.
    pub admin_key: VerificationKey,
    /// Public origin used to rebuild the signed `@target-uri` of every admin request.
    pub public_origin: PublicOrigin,
    /// Current attestation provider.
    pub attestor: Arc<dyn Attestor>,
    /// Validated current-price provider for rate-lock creation.
    pub rate_lock_quotes: Arc<dyn QuoteProvider>,
    /// Rate limit of anonymous quote reads by `client_secret`.
    pub client_reads: Arc<ClientReadLimiter>,
    /// Per-account and platform rate limits of authenticated merchant requests.
    pub rate_limits: Arc<ApiRateLimiter>,
    /// Sanctions screening of refund destinations and treasuries.
    pub screening: Arc<dyn DestinationScreener>,
    /// EIP-1271 checks of contract treasuries' proofs.
    pub contract_signatures: Arc<dyn ContractSignatures>,
}

impl AppState {
    /// The route a refund of the scope's deposit is checked against: the current version of the
    /// deposit's route, or, for a deposit of an asset without a route, the first current route of
    /// its chain in the scope's mode. A deposit outside the scope is `404`.
    pub(crate) async fn refund_route(
        &self,
        scope: Scope,
        deposit_id: uuid::Uuid,
    ) -> Result<&RouteFile, error::ApiError> {
        let (route, chain_id) = sqlx::query_as::<_, (Option<String>, i64)>(
            "SELECT route, chain_id FROM deposits WHERE id = $1 AND account_id = $2 AND livemode = $3",
        )
        .bind(deposit_id)
        .bind(scope.account_id())
        .bind(scope.livemode())
        .fetch_optional(&self.pool)
        .await?
        .ok_or_else(|| error::ApiError::not_found().with_param("deposit"))?;
        let chain_id = u64::try_from(chain_id).map_err(|_| error::ApiError::internal())?;
        let mut current = self.routes.current_in(scope.livemode());
        match route {
            Some(name) => current.find(|route| route.route == name),
            None => current.find(|route| route.chain.chain_id == chain_id),
        }
        .ok_or_else(|| {
            tracing::error!(%deposit_id, "the deposit's route is not loaded");
            error::ApiError::internal()
        })
    }
}

/// The API representation of the object an event is about, as `GET /v1/deposits/{id}`,
/// `GET /v1/quotes/{id}`, `GET /v1/refunds/{id}`, `GET /v1/api_keys/{id}`,
/// `GET /v1/treasuries/{id}`, `GET /v1/webhook_endpoints/{id}`, or `GET /v1/account` returns it to
/// `scope`, read on `connection` (the transaction recording the event). `Ok(None)` means the
/// object does not exist in `scope`; `Err(())` means rendering failed and was logged.
pub(crate) async fn render_object(
    connection: &mut sqlx::PgConnection,
    routes: &RouteSet,
    scope: Scope,
    object: crate::db::EventObject,
) -> Result<Option<serde_json::Value>, ()> {
    let rendered = match object {
        crate::db::EventObject::Deposit(id) => {
            deposits::find_deposit(&mut *connection, routes, scope, id)
                .await
                .map(|deposit| deposit.map(serde_json::to_value))
        }
        crate::db::EventObject::Quote(id) => quotes::find_quote(connection, routes, scope, id)
            .await
            .map(|quote| quote.map(serde_json::to_value)),
        crate::db::EventObject::Refund(id) => deposits::find_refund(&mut *connection, scope, id)
            .await
            .map(|refund| refund.map(serde_json::to_value)),
        crate::db::EventObject::ApiKey(id) => crate::api_keys::get(&mut *connection, scope, id)
            .await
            .map(|key| key.map(|key| serde_json::to_value(keys::api_key_object(&key, None))))
            .map_err(error::ApiError::from),
        crate::db::EventObject::Treasury(id) => crate::treasuries::get_in(connection, scope, id)
            .await
            .map(|treasury| {
                treasury
                    .map(|treasury| serde_json::to_value(treasuries::treasury_object(&treasury)))
            })
            .map_err(|_| error::ApiError::internal()),
        crate::db::EventObject::Account(id) if id == scope.account_id() => {
            account::find_account(connection, routes, scope)
                .await
                .map(|account| account.map(serde_json::to_value))
        }
        crate::db::EventObject::Account(_) => Ok(None),
        crate::db::EventObject::WebhookEndpoint(id) => {
            crate::webhook_endpoints::find_any(connection, scope, id)
                .await
                .map(|endpoint| endpoint.map(serde_json::to_value))
                .map_err(error::ApiError::from)
        }
    };
    match rendered {
        Ok(Some(Ok(value))) => Ok(Some(value)),
        Ok(None) => Ok(None),
        Ok(Some(Err(error))) => {
            tracing::error!(%error, "event object serialization failed");
            Err(())
        }
        Err(_) => {
            tracing::error!(object = ?object, "event object rendering failed");
            Err(())
        }
    }
}

/// Lets a payer's page on any origin read a `client_secret` view, with the `Request-Id` and
/// `Retry-After` of its responses (CORS; the view carries no credentials).
pub(crate) fn allow_cross_origin(response: &mut Response) {
    let headers = response.headers_mut();
    headers.insert(
        axum::http::header::ACCESS_CONTROL_ALLOW_ORIGIN,
        axum::http::HeaderValue::from_static("*"),
    );
    headers.insert(
        axum::http::header::ACCESS_CONTROL_EXPOSE_HEADERS,
        axum::http::HeaderValue::from_static("Request-Id, Retry-After"),
    );
}

/// The `Idempotency-Key` of a request when it is valid, for the events the request causes.
pub(crate) fn idempotency_key_of(headers: &axum::http::HeaderMap) -> Option<String> {
    extract::idempotency_key(headers).ok().flatten()
}

/// The merchant routes authenticated by a secret key.
fn merchant_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(quotes::get_config))
        .routes(routes!(quotes::list_quotes, quotes::create_quote))
        .routes(routes!(quotes::update_quote))
        .routes(routes!(quotes::cancel_quote))
        .routes(routes!(
            deposit_addresses::list_deposit_addresses,
            deposit_addresses::create_deposit_address
        ))
        .routes(routes!(deposit_addresses::update_deposit_address))
        .routes(routes!(deposit_addresses::rotate_deposit_address))
        .routes(routes!(deposits::list_deposits))
        .routes(routes!(deposits::get_deposit, deposits::update_deposit))
        .routes(routes!(deposits::list_refunds, deposits::create_refund))
        .routes(routes!(deposits::get_refund, deposits::update_refund))
        .routes(routes!(deposits::mark_refund_paid))
        .routes(routes!(deposits::cancel_refund))
        .routes(routes!(account::get_account, account::update_account))
        .routes(routes!(account::pause_account))
        .routes(routes!(account::resume_account))
        .routes(routes!(account::roll_webhook_key))
        .routes(routes!(account::get_attestation))
        .routes(routes!(keys::list_api_keys, keys::create_api_key))
        .routes(routes!(keys::get_api_key, keys::revoke_api_key))
        .routes(routes!(keys::roll_api_key))
        .routes(routes!(treasuries::create_treasury_challenge))
        .routes(routes!(
            treasuries::list_treasuries,
            treasuries::create_treasury
        ))
        .routes(routes!(treasuries::get_treasury))
        .routes(routes!(treasuries::cancel_treasury))
        .routes(routes!(treasuries::pause_treasury))
        .routes(routes!(treasuries::resume_treasury))
        .routes(routes!(
            webhook_endpoints::list_webhook_endpoints,
            webhook_endpoints::create_webhook_endpoint
        ))
        .routes(routes!(
            webhook_endpoints::get_webhook_endpoint,
            webhook_endpoints::update_webhook_endpoint,
            webhook_endpoints::delete_webhook_endpoint
        ))
        .routes(routes!(webhook_endpoints::test_webhook_endpoint))
        .routes(routes!(sweeps::get_balance))
        .routes(routes!(sweeps::list_sweeps))
        .routes(routes!(sweeps::list_forwarders))
        .routes(routes!(events::list_events))
        .routes(routes!(events::get_event))
        .routes(routes!(events::resend_event))
<<<<<<< HEAD
}
=======
        // Every merchant POST is idempotent by `Idempotency-Key`; authentication runs first.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::idempotent_post,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_merchant,
        ))
        // Outermost: while frozen after a restore, a write is refused before anything else runs,
        // so no idempotency key stores the refusal.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            refuse_writes_while_frozen,
        ));
>>>>>>> c0ce0cb (feat: restore mode after a restore from backup)

/// The routes a quote's or deposit address's `client_secret` also reads.
fn client_secret_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(quotes::get_quote))
        .routes(routes!(deposit_addresses::get_deposit_address))
}

/// The operator's routes, authenticated by RFC 9421 signatures.
fn admin_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(handlers::create_account))
        .routes(routes!(handlers::update_account))
        .routes(routes!(handlers::issue_api_key))
        .routes(routes!(handlers::admin_get_deposit))
        .routes(routes!(handlers::pause_account))
        .routes(routes!(handlers::resume_account))
        .routes(routes!(handlers::pause_customer))
        .routes(routes!(handlers::resume_customer))
        .routes(routes!(handlers::pause_treasury))
        .routes(routes!(handlers::resume_treasury))
        .routes(routes!(handlers::pause_route))
        .routes(routes!(handlers::resume_route))
        .routes(routes!(handlers::nudge_deposit))
        .routes(routes!(handlers::lift_reconciliation_block))
        .routes(routes!(handlers::daily_report))
        .routes(routes!(handlers::metrics))
<<<<<<< HEAD
}

/// utoipa's merchant and admin documents, before [`openapi`] finishes them.
#[cfg(test)]
fn documents() -> (utoipa::openapi::OpenApi, utoipa::openapi::OpenApi) {
    let (_, merchant) = merchant_routes()
        .merge(client_secret_routes())
        .split_for_parts();
    let (_, admin) = admin_routes().split_for_parts();
    (merchant, admin)
}

/// The finished OpenAPI documents: the merchant API's and the operator's.
#[derive(Clone, Debug)]
pub struct ApiDocs {
    /// `openapi.json`: the merchant API.
    pub merchant: serde_json::Value,
    /// `openapi.admin.json`: the admin API.
    pub admin: serde_json::Value,
}

/// Builds the Axum router, serving both OpenAPI documents, and the documents.
pub fn router(state: AppState) -> (Router, ApiDocs) {
    // Every merchant POST is idempotent by `Idempotency-Key`; authentication runs first.
    let merchant = merchant_routes()
=======
        .routes(routes!(restore::get_restore))
        .routes(routes!(restore::revoke_api_key))
        .routes(routes!(restore::verify_treasuries))
        .routes(routes!(restore::delete_webhook_endpoint))
        .routes(routes!(restore::reissue_deposit_address))
        .routes(routes!(restore::import_events))
        .routes(routes!(restore::unfreeze))
>>>>>>> c0ce0cb (feat: restore mode after a restore from backup)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::idempotent_post,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_merchant,
        ));
    // A quote and a deposit address are also readable without an API key by a `client_secret`.
    let client_secret = client_secret_routes().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::authenticate_merchant_or_client_secret,
    ));
    let admin = admin_routes().route_layer(middleware::from_fn_with_state(
        state.clone(),
        auth::authenticate_admin,
    ));
    let (merchant_router, merchant_doc) = merchant.merge(client_secret).split_for_parts();
    let (admin_router, admin_doc) = admin.split_for_parts();
    let docs = ApiDocs {
        merchant: openapi::merchant(&merchant_doc),
        admin: openapi::admin(&admin_doc),
    };
    let router = merchant_router
        .merge(admin_router)
        .route("/healthz", get(healthz))
        .with_state(state)
        .route("/openapi.json", get(serve_openapi))
        .route("/openapi.admin.json", get(serve_admin_openapi))
        .layer(middleware::from_fn(crate::observability::request_context))
        .layer(Extension(Arc::new(docs.clone())));
    (router, docs)
}

/// Seconds `Retry-After` asks a client to wait before retrying a write refused while the service
/// is frozen after a restore: reconciliation takes minutes to hours.
const RESTORE_RETRY_AFTER_SECONDS: u32 = 300;

/// `503 service_restoring` with `Retry-After`.
fn restoring() -> Response {
    let mut response = error::ApiError::service_restoring().into_response();
    response.headers_mut().insert(
        header::RETRY_AFTER,
        HeaderValue::from(RESTORE_RETRY_AFTER_SECONDS),
    );
    response
}

/// Refuses every merchant write while the service is frozen after a restore
/// (`crate::restore_mode`); reads pass.
async fn refuse_writes_while_frozen(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return next.run(request).await;
    }
    match crate::restore_mode::is_frozen(&state.pool).await {
        Ok(false) => next.run(request).await,
        Ok(true) => restoring(),
        Err(error) => error::ApiError::from(error).into_response(),
    }
}

/// Builds the router of an instance restored from backup (`TOPUP_SERVICE_ENABLED=read-only`,
/// `deploy/RESTORE.md`): every request other than `GET`, `HEAD`, and the operator's restore
/// reconciliation (`/v1/admin/restore/…`) is refused with `503 service_restoring`, and `/healthz`
/// reports the boot-time `restore-check` result read from `restore_report`.
pub fn read_only_router(state: AppState, restore_report: Option<PathBuf>) -> Router {
    let (router, _) = router(state);
    router
        .layer(middleware::from_fn(reject_writes))
        .layer(Extension(ReadOnly {
            restore_report: restore_report.map(Arc::from),
        }))
}

/// Marks a read-only router and locates its restore-check report.
#[derive(Clone)]
struct ReadOnly {
    restore_report: Option<Arc<Path>>,
}

async fn reject_writes(request: Request, next: Next) -> Response {
    if matches!(*request.method(), Method::GET | Method::HEAD)
        || request.uri().path().starts_with("/v1/admin/restore/")
    {
        next.run(request).await
    } else {
        restoring()
    }
}

/// The merchant API's deterministic pretty-printed OpenAPI document, `openapi.json`.
pub fn openapi_json(state: AppState) -> Result<String, serde_json::Error> {
    let (_, docs) = router(state);
    serde_json::to_string_pretty(&docs.merchant).map(|json| format!("{json}\n"))
}

/// The admin API's deterministic pretty-printed OpenAPI document, `openapi.admin.json`.
pub fn openapi_admin_json(state: AppState) -> Result<String, serde_json::Error> {
    let (_, docs) = router(state);
    serde_json::to_string_pretty(&docs.admin).map(|json| format!("{json}\n"))
}

async fn serve_openapi(Extension(docs): Extension<Arc<ApiDocs>>) -> Json<serde_json::Value> {
    Json(docs.merchant.clone())
}

async fn serve_admin_openapi(Extension(docs): Extension<Arc<ApiDocs>>) -> Json<serde_json::Value> {
    Json(docs.admin.clone())
}

async fn healthz(
    State(state): State<AppState>,
    read_only: Option<Extension<ReadOnly>>,
) -> Response {
    let status = match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(1) => StatusCode::OK,
        Ok(_) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    };
    let Some(Extension(read_only)) = read_only else {
        return status.into_response();
    };
    let restore_check = match &read_only.restore_report {
        Some(path) => read_restore_report(path).await,
        None => serde_json::Value::Null,
    };
    (
        status,
        Json(serde_json::json!({ "mode": "read-only", "restore_check": restore_check })),
    )
        .into_response()
}

/// The restore-check report, or `null` while it has not been written.
async fn read_restore_report(path: &Arc<Path>) -> serde_json::Value {
    let file = Arc::clone(path);
    let read = tokio::task::spawn_blocking(move || std::fs::read(&file))
        .await
        .unwrap_or_else(|error| Err(std::io::Error::other(error)));
    match read {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|_| {
            tracing::warn!(path = %path.display(), "restore-check report is not valid JSON");
            serde_json::Value::Null
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => serde_json::Value::Null,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "failed to read restore-check report");
            serde_json::Value::Null
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::SigningKey;
    use tower::ServiceExt as _;

    use super::{AppState, VerificationKey};

    #[tokio::test]
    async fn unknown_paths_are_not_found_with_a_request_id() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1/unused")
            .expect("lazy pool URL is valid");
        let admin_key = SigningKey::from_bytes(&[1; 32]);
        let state = AppState {
            pool,
            routes: Arc::default(),
            admin_key: VerificationKey::from_base64(
                "admin/v1".to_owned(),
                &STANDARD.encode(admin_key.verifying_key().as_bytes()),
            )
            .expect("admin key is valid"),
            public_origin: super::PublicOrigin::parse("http://api.test")
                .expect("test origin is valid"),
            attestor: Arc::new(topup_adapters::attestation::DstackAttestor::new()),
            rate_lock_quotes: Arc::new(crate::locks::UnavailableQuoteProvider),
            client_reads: Arc::default(),
            rate_limits: Arc::default(),
            screening: Arc::new(crate::refunds::UnavailableDestinationScreener),
            contract_signatures: Arc::new(crate::treasuries::UnavailableContractSignatures),
        };
        let response = super::router(state)
            .0
            .oneshot(
                Request::builder()
                    .uri("/unknown")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("public request succeeds");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert!(
            response
                .headers()
                .get("request-id")
                .and_then(|id| id.to_str().ok())
                .is_some_and(|id| id.starts_with("req_") && id.len() == 36)
        );
        let _ = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads");
    }
}
