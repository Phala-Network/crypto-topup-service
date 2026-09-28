//! Idempotent requests (design §13), Stripe's model: every merchant `POST` accepts an
//! `Idempotency-Key`, stored per `(account, livemode, key)` with a fingerprint of the request and
//! its first response for 24 hours. A repeat with the same request gets that response again with
//! `Idempotent-Replayed: true`; a repeat with another request is `400 idempotency_error`; a repeat
//! while the first request still runs is `409 idempotency_key_in_use`.
//!
//! As Stripe's, the result is saved once the handler starts executing, whatever it is, including
//! a `500`: a retry after a failure whose effects are unknown replays it rather than running the
//! request twice (<https://docs.stripe.com/api/idempotent_requests>). A request that did not
//! execute is not saved, so a retry with the same key runs it: one that failed validation
//! (`parameter_*`), was rate limited (`429`), or met a temporary unavailability (`503`), marked
//! [`NotExecuted`]. An API key's `secret` is never stored: a replayed key creation or roll
//! returns the key without it. A quote's `client_secret` is stored with its response; it reads
//! only the quote's public view, which anyone holding the database can read anyway. A key held by
//! a request that never finished (the process stopped) is released to a repeat of the same request
//! after a minute.

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::PgPool;

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, NotExecuted};
use super::extract::idempotency_key;
use crate::tenancy::Scope;

/// Largest request or response body the layer reads.
const MAX_BODY_BYTES: usize = 1_048_576;
const REPLAYED_HEADER: &str = "idempotent-replayed";

/// Marks a response whose top-level `secret` must not be stored for replay.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ContainsSecret;

/// Serves a repeated merchant `POST` from `idempotency_keys`; runs after authentication.
pub(crate) async fn idempotent_post(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if request.method() != Method::POST {
        return next.run(request).await;
    }
    let key = match idempotency_key(request.headers()) {
        Ok(Some(key)) => key,
        Ok(None) => return next.run(request).await,
        Err(error) => return error.into_response(),
    };
    let Some(scope) = request
        .extensions()
        .get::<Merchant>()
        .map(|merchant| merchant.scope)
    else {
        tracing::error!("idempotency layer ran before authentication");
        return ApiError::internal().into_response();
    };
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, MAX_BODY_BYTES).await else {
        return ApiError::bad_request("the request body is too large").into_response();
    };
    let fingerprint = fingerprint(
        &parts.method,
        parts
            .uri
            .path_and_query()
            .map_or("/", |value| value.as_str()),
        &body,
    );
    match claim(&state.pool, scope, &key, &fingerprint).await {
        Ok(Claim::Claimed) => {}
        Ok(Claim::Replay(stored)) => return replay(&stored),
        Ok(Claim::InUse) => return ApiError::idempotency_key_in_use().into_response(),
        Ok(Claim::OtherRequest) => return ApiError::idempotency_key_reused().into_response(),
        Err(error) => return ApiError::from(error).into_response(),
    }

    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    let status = response.status();
    if response.extensions().get::<NotExecuted>().is_some() {
        release(&state.pool, scope, &key).await;
        return response;
    }
    let contains_secret = response.extensions().get::<ContainsSecret>().is_some();
    let (parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "response body could not be read for idempotency");
            release(&state.pool, scope, &key).await;
            return ApiError::internal().into_response();
        }
    };
    store(&state.pool, scope, &key, status, &bytes, contains_secret).await;
    Response::from_parts(parts, Body::from(bytes))
}

/// SHA-256 over the method, the path and query, and the body.
fn fingerprint(method: &Method, target: &str, body: &[u8]) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(method.as_str().as_bytes());
    digest.update([0]);
    digest.update(target.as_bytes());
    digest.update([0]);
    digest.update(body);
    digest.finalize().into()
}

enum Claim {
    /// This request holds the key and runs.
    Claimed,
    /// The stored response of the same request.
    Replay(Value),
    /// The same request is still running.
    InUse,
    /// The key was used with another request.
    OtherRequest,
}

async fn claim(
    pool: &PgPool,
    scope: Scope,
    key: &str,
    fingerprint: &[u8; 32],
) -> Result<Claim, sqlx::Error> {
    sqlx::query("DELETE FROM idempotency_keys WHERE created_at < now() - interval '24 hours'")
        .execute(pool)
        .await?;
    // A key whose request never stored a response is taken over by the same request after a
    // minute; an expired row not yet pruned by a concurrent request is replaced.
    let claimed = sqlx::query(
        r#"
        INSERT INTO idempotency_keys (account_id, livemode, key, fingerprint)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (account_id, livemode, key) DO UPDATE
            SET fingerprint = EXCLUDED.fingerprint, response = NULL, created_at = now()
            WHERE idempotency_keys.created_at < now() - interval '24 hours'
               OR (idempotency_keys.response IS NULL
                   AND idempotency_keys.fingerprint = EXCLUDED.fingerprint
                   AND idempotency_keys.created_at < now() - interval '1 minute')
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(key)
    .bind(fingerprint.as_slice())
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    if claimed {
        return Ok(Claim::Claimed);
    }
    let stored = sqlx::query_as::<_, (Vec<u8>, Option<Value>)>(
        "SELECT fingerprint, response FROM idempotency_keys \
         WHERE account_id = $1 AND livemode = $2 AND key = $3",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(key)
    .fetch_optional(pool)
    .await?;
    Ok(match stored {
        // Released between the two statements: the client retries.
        None => Claim::InUse,
        Some((stored, _)) if stored.as_slice() != fingerprint.as_slice() => Claim::OtherRequest,
        Some((_, None)) => Claim::InUse,
        Some((_, Some(response))) => Claim::Replay(response),
    })
}

async fn store(
    pool: &PgPool,
    scope: Scope,
    key: &str,
    status: StatusCode,
    body: &Bytes,
    contains_secret: bool,
) {
    let mut body = match serde_json::from_slice::<Value>(body) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "a merchant POST answered with a non-JSON body");
            release(pool, scope, key).await;
            return;
        }
    };
    if contains_secret && let Some(object) = body.as_object_mut() {
        object.remove("secret");
    }
    let stored = serde_json::json!({"status": status.as_u16(), "body": body});
    let result = sqlx::query(
        "UPDATE idempotency_keys SET response = $4 \
         WHERE account_id = $1 AND livemode = $2 AND key = $3",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(key)
    .bind(stored)
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, "idempotent response was not stored");
        release(pool, scope, key).await;
    }
}

/// Deletes the key's row so a retry runs the request again.
async fn release(pool: &PgPool, scope: Scope, key: &str) {
    let result = sqlx::query(
        "DELETE FROM idempotency_keys \
         WHERE account_id = $1 AND livemode = $2 AND key = $3 AND response IS NULL",
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(key)
    .execute(pool)
    .await;
    if let Err(error) = result {
        tracing::error!(%error, "idempotency key was not released; it frees after a minute");
    }
}

fn replay(stored: &Value) -> Response {
    let status = stored
        .get("status")
        .and_then(Value::as_u64)
        .and_then(|status| u16::try_from(status).ok())
        .and_then(|status| StatusCode::from_u16(status).ok());
    let (Some(status), Some(body)) = (status, stored.get("body")) else {
        tracing::error!("stored idempotent response is malformed");
        return ApiError::internal().into_response();
    };
    let mut response = (status, axum::Json(body)).into_response();
    let headers = response.headers_mut();
    headers.insert(REPLAYED_HEADER, HeaderValue::from_static("true"));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fingerprint_covers_method_target_and_body() {
        let base = fingerprint(&Method::POST, "/v1/quotes", b"{}");
        assert_eq!(base, fingerprint(&Method::POST, "/v1/quotes", b"{}"));
        assert_ne!(base, fingerprint(&Method::POST, "/v1/refunds", b"{}"));
        assert_ne!(base, fingerprint(&Method::POST, "/v1/quotes", b"{ }"));
        assert_ne!(base, fingerprint(&Method::PUT, "/v1/quotes", b"{}"));
        // The separators keep the parts apart.
        assert_ne!(
            fingerprint(&Method::POST, "/a", b"b"),
            fingerprint(&Method::POST, "/ab", b"")
        );
    }
}
