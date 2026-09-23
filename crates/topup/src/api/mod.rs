//! Axum API, RFC 9421 authentication, and generated OpenAPI.

mod attestation;
mod auth;
mod error;
mod handlers;
pub mod models;
mod rate_locks;
mod repository;

use std::sync::Arc;

use crate::locks::QuoteProvider;
use axum::extract::{Extension, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::get;
use axum::{Json, Router};
use sqlx::PgPool;
use topup_core::route::RouteFile;
use utoipa::openapi::security::{ApiKey, ApiKeyValue, SecurityScheme};
use utoipa::openapi::{Info, OpenApi};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

pub use attestation::{AttestationError, AttestationFuture, Attestor, UnavailableAttestor};
pub use auth::VerificationKey;
pub use topup_adapters::http_signature::PublicOrigin;

/// Shared state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Application-role PostgreSQL connection pool.
    pub pool: PgPool,
    /// Attested route configurations.
    pub routes: Arc<Vec<RouteFile>>,
    /// Separately configured administrative verification key.
    pub admin_key: VerificationKey,
    /// Public origin used to rebuild the signed `@target-uri` of every request.
    pub public_origin: PublicOrigin,
    /// Current attestation provider.
    pub attestor: Arc<dyn Attestor>,
    /// Validated current-price provider for rate-lock creation.
    pub rate_lock_quotes: Arc<dyn QuoteProvider>,
}

impl AppState {
    pub(crate) fn route_for_product<'a>(
        &'a self,
        product: &crate::db::Product,
    ) -> Result<&'a RouteFile, error::ApiError> {
        let mut routes = self
            .routes
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
        .routes(routes!(handlers::register_account))
        .routes(routes!(
            handlers::get_deposit_address,
            handlers::create_deposit_address
        ))
        .routes(routes!(handlers::rotate_deposit_address))
        .routes(routes!(rate_locks::create_rate_lock))
        .routes(routes!(
            rate_locks::get_rate_lock,
            rate_locks::cancel_rate_lock
        ))
        .routes(routes!(handlers::list_deposits))
        .routes(routes!(handlers::get_deposit))
        .routes(routes!(handlers::lookup_deposits))
        .routes(routes!(handlers::get_limits))
        .routes(routes!(handlers::pause_account))
        .routes(routes!(handlers::resume_account))
        .routes(routes!(handlers::request_refund))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_product,
        ));

    let admin = OpenApiRouter::new()
        .routes(routes!(handlers::pause_route))
        .routes(routes!(handlers::resume_route))
        .routes(routes!(handlers::nudge_deposit))
        .routes(routes!(handlers::approve_refund))
        .routes(routes!(handlers::record_refund))
        .routes(routes!(handlers::daily_report))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth::authenticate_admin,
        ));

    let mut documented = OpenApiRouter::new()
        .merge(product)
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
        .layer(Extension(document));
    (router, openapi)
}

/// Returns the deterministic pretty-printed OpenAPI snapshot.
pub fn openapi_json(state: AppState) -> Result<String, serde_json::Error> {
    let (_, openapi) = router(state);
    serde_json::to_string_pretty(&openapi).map(|json| format!("{json}\n"))
}

async fn serve_openapi(Extension(openapi): Extension<Arc<OpenApi>>) -> Json<OpenApi> {
    Json((*openapi).clone())
}

async fn healthz(State(state): State<AppState>) -> StatusCode {
    match sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&state.pool)
        .await
    {
        Ok(1) => StatusCode::OK,
        Ok(_) | Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use base64::Engine as _;
    use base64::engine::general_purpose::STANDARD;
    use ed25519_dalek::SigningKey;

    use super::{AppState, UnavailableAttestor, VerificationKey};
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
            routes: Arc::new(vec![newer.clone(), route]),
            admin_key: VerificationKey::from_base64(
                "admin/v1".to_owned(),
                &STANDARD.encode(admin_key.verifying_key().as_bytes()),
            )
            .expect("admin key is valid"),
            public_origin: super::PublicOrigin::parse("http://api.test")
                .expect("test origin is valid"),
            attestor: Arc::new(UnavailableAttestor),
            rate_lock_quotes: Arc::new(crate::locks::UnavailableQuoteProvider),
        };
        let product = Product {
            id: Uuid::nil(),
            slug: "phala-cloud".to_owned(),
            settlement_url: String::new(),
            webhook_url: String::new(),
            pubkey: String::new(),
            kid: String::new(),
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
}
