use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use uuid::Uuid;

use crate::audit::RequestRef;

/// Stripe's request id header (<https://docs.stripe.com/api/request_ids>).
static REQUEST_ID: HeaderName = HeaderName::from_static("request-id");

/// Gives every API request an id, `req_` and 32 hex digits, returned as `Request-Id`, recorded on
/// the events the request causes with its `Idempotency-Key` (a [`RequestRef`] extension), and
/// runs the request in a route-labelled span.
pub async fn request_context(mut request: Request, next: Next) -> Response {
    let request_id = format!("req_{}", Uuid::new_v4().simple());
    // An invalid key is refused by the idempotency layer; it names no request here.
    let idempotency_key = crate::api::idempotency_key_of(request.headers());
    request.extensions_mut().insert(RequestRef {
        id: request_id.clone(),
        idempotency_key,
    });
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_else(|| request.uri().path());
    let method = request.method().clone();
    let span = tracing::info_span!(
        "api.request",
        request_id = %request_id,
        route,
        method = %method,
    );
    let mut response = next.run(request).instrument(span).await;
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID.clone(), value);
    }
    response
}
