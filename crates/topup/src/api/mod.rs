//! Axum API, RFC 9421 authentication, and generated OpenAPI.

mod attestation;
mod auth;
mod error;
mod handlers;
pub mod models;
mod repository;

use std::sync::Arc;

use axum::extract::Extension;
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

/// Shared state for all API handlers.
#[derive(Clone)]
pub struct AppState {
    /// Application-role PostgreSQL connection pool.
    pub pool: PgPool,
    /// Attested route configurations.
    pub routes: Arc<Vec<RouteFile>>,
    /// Separately configured administrative verification key.
    pub admin_key: VerificationKey,
    /// Current attestation provider.
    pub attestor: Arc<dyn Attestor>,
}

impl AppState {
    fn route_for_product<'a>(
        &'a self,
        product: &crate::db::Product,
    ) -> Result<&'a RouteFile, error::ApiError> {
        let mut routes = self
            .routes
            .iter()
            .filter(|route| route.destination.product == product.slug);
        let route = routes.next().ok_or_else(error::ApiError::not_found)?;
        if routes.next().is_some() {
            return Err(error::ApiError::conflict(
                "multiple routes match this product; the address API requires one active route",
            ));
        }
        Ok(route)
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
        .routes(routes!(handlers::create_rate_lock))
        .routes(routes!(handlers::get_rate_lock, handlers::cancel_rate_lock))
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
            SecurityScheme::ApiKey(ApiKey::Header(ApiKeyValue::new("Signature"))),
        );
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
