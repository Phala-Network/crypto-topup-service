//! Axum API, RFC 9421 authentication, and generated OpenAPI.

mod attestation;
mod auth;
mod client_limit;
mod error;
mod extract;
mod handlers;
pub mod models;
mod pending;
mod quotes;
mod repository;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::locks::QuoteProvider;
use crate::routes::RouteSet;
use axum::extract::{Extension, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse as _, Response};
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use topup_core::route::RouteFile;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use attestation::{AttestationError, AttestationFuture, Attestor};
pub use auth::VerificationKey;
pub use client_limit::ClientReadLimiter;
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
    /// Public origin used to rebuild the signed `@target-uri` of every request.
    pub public_origin: PublicOrigin,
    /// Current attestation provider.
    pub attestor: Arc<dyn Attestor>,
    /// Validated current-price provider for rate-lock creation.
    pub rate_lock_quotes: Arc<dyn QuoteProvider>,
    /// Rate limit of unsigned quote reads by `client_secret`.
    pub client_reads: Arc<ClientReadLimiter>,
}

impl AppState {
    pub(crate) fn route_for_product<'a>(
        &'a self,
        product: &crate::db::Product,
    ) -> Result<&'a RouteFile, error::ApiError> {
        let mut routes = self
            .routes
            .routes()
            .iter()
            .filter(|route| route.destination.product == product.slug);
        let first = routes.next().ok_or_else(error::ApiError::not_found)?;
        let mut current = first;
        for route in routes {
            if route.route != first.route {
                return Err(error::ApiError::conflict(
                    "multiple active routes match this product",
                ));
            }
            if route.version > current.version {
                current = route;
            }
        }
        Ok(current)
    }
}

/// Builds the authenticated Axum router and its OpenAPI document.
pub fn router(state: AppState) -> (Router, OpenApi) {
    let product = OpenApiRouter::new()
        .routes(routes!(quotes::get_config))
        .routes(routes!(quotes::create_quote))
        .routes(routes!(quotes::cancel_quote))
        .routes(routes!(
            handlers::get_deposit_address,
            handlers::create_deposit_address
        ))
        .routes(routes!(handlers::rotate_deposit_address))
        .routes(routes!(handlers::list_deposits))
        .routes(routes!(pending::list_pending_deposits))
        .routes(routes!(handlers::get_deposit))
        .routes(routes!(handlers::lookup_deposits))
        .routes(routes!(handlers::pause_account))
        .routes(routes!(handlers::resume_account))
        .routes(routes!(handlers::request_refund))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_product,
        ));

    let admin = OpenApiRouter::new()
        .routes(routes!(handlers::register_product))
        .routes(routes!(handlers::update_product))
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

    // A quote is also readable without a signature by its `client_secret`.
    let quote = OpenApiRouter::new()
        .routes(routes!(quotes::get_quote))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_product_or_client_secret,
        ));

    let mut documented = OpenApiRouter::new()
        .merge(product)
        .merge(quote)
        .merge(admin)
        .routes(routes!(handlers::get_attestation));
    let mut info = Info::new("Crypto Top-up Service API", env!("CARGO_PKG_VERSION"));
    info.description = Some(
        "Authenticated product and administrative API for deterministic crypto top-ups.".to_owned(),
    );
    documented.get_openapi_mut().info = info;
    documented
        .get_openapi_mut()
        .components
        .get_or_insert_default()
        .add_security_scheme(
            "http_message_signature",
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::with_description(
                "Signature",
                "RFC 9421 ed25519 signature over `@method`, `@target-uri`, `content-digest`, and \
                 `idempotency-key` when sent. `@target-uri` is the service's configured public \
                 origin (`TOPUP_PUBLIC_ORIGIN`) followed by the request path and query, so sign \
                 the public URL you call; `Host` and `X-Forwarded-*` headers are ignored.",
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

    use super::{AppState, RouteSet, VerificationKey};
    use crate::db::Product;
    use topup_core::route::RouteFile;
    use uuid::Uuid;

    #[tokio::test]
    async fn current_product_route_is_the_highest_loaded_version() {
        let route: RouteFile =
            serde_saphyr::from_str(include_str!("../../tests/fixtures/phala-cloud-pha.yaml"))
                .expect("route fixture parses");
        let mut newer = route.clone();
        newer.version = route.version + 1;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://unused:unused@127.0.0.1/unused")
            .expect("lazy pool URL is valid");
        let admin_key = SigningKey::from_bytes(&[1; 32]);
        let state = AppState {
            pool,
            routes: Arc::new(RouteSet::new(vec![newer.clone(), route]).expect("routes load")),
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
        };
        let product = Product {
            id: Uuid::nil(),
            slug: "phala-cloud".to_owned(),
            webhook_url: String::new(),
            pubkey: String::new(),
            paused_scopes: Vec::new(),
        };

        assert_eq!(
            state
                .route_for_product(&product)
                .expect("product route exists")
                .version,
            newer.version
        );
    }

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
