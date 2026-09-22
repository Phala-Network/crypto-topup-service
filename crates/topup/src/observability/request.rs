use axum::extract::{MatchedPath, Request};
use axum::http::{HeaderName, HeaderValue};
use axum::middleware::Next;
use axum::response::Response;
use tracing::Instrument as _;
use uuid::Uuid;

static REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// Adds a generated request id and runs the API request in a route-labelled span.
pub async fn request_context(request: Request, next: Next) -> Response {
    let request_id = Uuid::new_v4();
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
    if let Ok(value) = HeaderValue::from_str(&request_id.to_string()) {
        response.headers_mut().insert(REQUEST_ID.clone(), value);
    }
    response
}
