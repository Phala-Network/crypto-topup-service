//! Axum API: merchant routes authenticated by API keys, the admin routes by RFC 9421
//! signatures, and the generated OpenAPI.

mod account;
mod attestation;
mod auth;
mod client_limit;
mod deposits;
mod error;
mod extract;
mod handlers;
mod idempotency;
mod keys;
mod metadata;
pub mod models;
mod pending;
mod quotes;
mod rate_limit;
mod repository;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::locks::QuoteProvider;
use crate::routes::RouteSet;
use crate::tenancy::Scope;
use axum::extract::{Extension, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use topup_core::route::RouteFile;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, HttpAuthScheme, HttpBuilder, SecurityScheme};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use attestation::{AttestationError, AttestationFuture, Attestor};
pub use auth::VerificationKey;
pub use client_limit::ClientReadLimiter;
pub use rate_limit::{ApiRateLimiter, RateLimits};
pub use topup_adapters::http_signature::PublicOrigin;

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

/// Renders an event's `data`, `{"object": …}`: the API representation of the object the event is
/// about, as returned by `GET /v1/deposits/{id}`, `GET /v1/quotes/{id}`, `GET /v1/api_keys/{id}`,
/// or `GET /v1/account` to the event's account and mode. `Ok(None)` means the object does not exist in `scope`; `Err(())` means rendering
/// failed and was logged.
pub(crate) async fn event_data(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    object: crate::db::EventObject,
) -> Result<Option<serde_json::Value>, ()> {
    let rendered = match object {
        crate::db::EventObject::Deposit(id) => deposits::find_deposit(pool, routes, scope, id)
            .await
            .map(|deposit| deposit.map(serde_json::to_value)),
        crate::db::EventObject::Quote(id) => quotes::find_quote(pool, routes, scope, id)
            .await
            .map(|quote| quote.map(serde_json::to_value)),
        crate::db::EventObject::ApiKey(id) => crate::api_keys::get(pool, scope, id)
            .await
            .map(|key| key.map(|key| serde_json::to_value(keys::api_key_object(&key, None))))
            .map_err(error::ApiError::from),
        crate::db::EventObject::Account(id) if id == scope.account_id() => {
            account::find_account(pool, scope)
                .await
                .map(|account| account.map(serde_json::to_value))
                .map_err(error::ApiError::from)
        }
        crate::db::EventObject::Account(_) => Ok(None),
    };
    match rendered {
        Ok(Some(Ok(value))) => Ok(Some(serde_json::json!({ "object": value }))),
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

/// Builds the authenticated Axum router and its OpenAPI document.
pub fn router(state: AppState) -> (Router, OpenApi) {
    let merchant = OpenApiRouter::new()
        .routes(routes!(quotes::get_config))
        .routes(routes!(quotes::create_quote))
        .routes(routes!(quotes::update_quote))
        .routes(routes!(quotes::cancel_quote))
        .routes(routes!(deposits::list_deposits))
        .routes(routes!(deposits::get_deposit, deposits::update_deposit))
        .routes(routes!(deposits::create_refund))
        .routes(routes!(deposits::get_refund, deposits::update_refund))
        .routes(routes!(account::get_account))
        .routes(routes!(keys::list_api_keys, keys::create_api_key))
        .routes(routes!(keys::get_api_key, keys::revoke_api_key))
        .routes(routes!(keys::roll_api_key))
        // Every merchant POST is idempotent by `Idempotency-Key`; authentication runs first.
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            idempotency::idempotent_post,
        ))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_merchant,
        ));

    let admin = OpenApiRouter::new()
        .routes(routes!(handlers::create_account))
        .routes(routes!(handlers::update_account))
        .routes(routes!(handlers::issue_api_key))
        .routes(routes!(handlers::admin_get_deposit))
        .routes(routes!(handlers::pause_customer))
        .routes(routes!(handlers::resume_customer))
        .routes(routes!(handlers::pause_route))
        .routes(routes!(handlers::resume_route))
        .routes(routes!(handlers::nudge_deposit))
        .routes(routes!(handlers::approve_refund))
        .routes(routes!(handlers::record_refund))
        .routes(routes!(handlers::lift_reconciliation_block))
        .routes(routes!(handlers::replay_outbox_event))
        .routes(routes!(handlers::daily_report))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_admin,
        ));

    // A quote is also readable without an API key by its `client_secret`.
    let quote = OpenApiRouter::new()
        .routes(routes!(quotes::get_quote))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_merchant_or_client_secret,
        ));

    let mut documented = OpenApiRouter::new()
        .merge(merchant)
        .merge(quote)
        .merge(admin)
        .routes(routes!(handlers::get_attestation));
    let mut info = Info::new("Phala Pay API", env!("CARGO_PKG_VERSION"));
    info.description = Some(
        "Authenticated merchant and administrative API for Phala Pay crypto payments.".to_owned(),
    );
    documented.get_openapi_mut().info = info;
    let components = documented
        .get_openapi_mut()
        .components
        .get_or_insert_default();
    components.add_security_scheme(
        "api_key",
        SecurityScheme::Http(
            HttpBuilder::new()
                .scheme(HttpAuthScheme::Bearer)
                .description(Some(
                    "A secret key, `Authorization: Bearer ppay_sk_test_…` or `ppay_sk_live_…`; \
                     the key selects the account and the mode. HTTP Basic is not accepted.",
                ))
                .build(),
        ),
    );
    components.add_security_scheme(
        "http_message_signature",
        SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
            "Signature",
            "The operator's admin API only: an RFC 9421 ed25519 signature over `@method`, \
             `@target-uri`, `content-digest`, and `idempotency-key` when sent. `@target-uri` is \
             the service's configured public origin (`TOPUP_PUBLIC_ORIGIN`) followed by the \
             request path and query, so sign the public URL you call; `Host` and \
             `X-Forwarded-*` headers are ignored.",
        ))),
    );
    let documented = documented.route("/healthz", get(healthz));
    let (router, openapi) = documented.with_state(state).split_for_parts();
    let document = Arc::new(openapi.clone());
    let router = router
        .route("/openapi.json", get(serve_openapi))
        .layer(middleware::from_fn(crate::observability::request_context))
        .layer(Extension(document));
    (router, openapi)
}

/// Builds the router of an instance restored from backup (`TOPUP_SERVICE_ENABLED=read-only`,
/// `deploy/RESTORE.md`): every request other than `GET` and `HEAD` is refused, and `/healthz`
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
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        next.run(request).await
    } else {
        error::ApiError::service_unavailable(
            "the service is read-only while a restored database is verified",
        )
        .into_response()
    }
}

/// Returns the deterministic pretty-printed OpenAPI snapshot.
pub fn openapi_json(state: AppState) -> Result<String, serde_json::Error> {
    let (_, openapi) = router(state);
    serde_json::to_string_pretty(&openapi).map(|json| format!("{json}\n"))
}

async fn serve_openapi(Extension(openapi): Extension<Arc<OpenApi>>) -> Json<OpenApi> {
    Json((*openapi).clone())
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
        assert!(response.headers().contains_key("x-request-id"));
        let _ = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads");
    }
}
