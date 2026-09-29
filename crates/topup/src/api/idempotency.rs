//! Idempotent requests (design §13), Stripe's model: every merchant `POST` accepts an
//! `Idempotency-Key`, stored per `(account, livemode, key)` with a fingerprint of the request and
//! its first response for 24 hours. A repeat with the same request gets that response again with
//! `Idempotent-Replayed: true`; a repeat with another request is `400 idempotency_error`; a repeat
//! while the first request still runs is `409 idempotency_key_in_use`.
//!
//! As Stripe's, the result is saved once the handler starts executing, whatever it is, including
//! a `500` (<https://docs.stripe.com/api/idempotent_requests>). A request that did not execute is
//! not saved, so a retry with the same key runs it: one refused by authentication or
//! authorization (both run before this layer), one that failed validation (`parameter_*`), was
//! rate limited (`429`), or met a temporary unavailability (`503`), marked [`NotExecuted`].
//!
//! The result commits with the request's changes, in one PostgreSQL transaction (Brandur Leach,
//! "Implementing Stripe-like Idempotency Keys in Postgres",
//! <https://brandur.org/idempotency-keys>). A request claims its key under a fresh `owner`. Its
//! handler makes its external calls first (a price fetch, sanctions screening), then opens the
//! transaction with [`Idempotent::begin`], which locks the key's row and checks the request still
//! owns it, makes its changes and renders its response in it, and saves the response to the row
//! and commits with [`Idempotent::commit`]. So a key has a saved response exactly when its
//! request's changes committed, whether or not the client received it. A key whose request never
//! saved a response (the process stopped, the client disconnected, or the request is still slow)
//! is taken over by a repeat of the same request after a minute under a new owner, which runs the
//! request again: nothing committed, and the former owner can no longer commit (it is fenced by
//! the row lock and the owner check). A response the handler returns without committing it, a
//! failure whose transaction rolled back, is saved afterwards if the request still owns the key.
//!
//! An API key's `secret` is never stored: a replayed key creation or roll returns the key without
//! it. A quote's `client_secret` is stored with its response; it reads only the quote's public
//! view, which anyone holding the database can read anyway.

use std::convert::Infallible;

use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{FromRequestParts, Request, State};
use axum::http::request::Parts;
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::Value;
use sha2::{Digest, Sha256};
use sqlx::{PgExecutor, PgPool, Postgres, Transaction};
use uuid::Uuid;

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

/// Marks a response [`Idempotent::commit`] saved with its request's changes.
#[derive(Clone, Copy, Debug)]
struct Saved;

/// A running request's hold on its key: the key's row, and the owner that claimed it.
#[derive(Clone, Debug)]
struct Claim {
    scope: Scope,
    key: String,
    owner: Uuid,
}

/// The `Idempotency-Key` a merchant `POST` holds, if it sent one: the handler opens the
/// transaction of its changes with [`Self::begin`] and commits it with its response through
/// [`Self::commit`].
pub(crate) struct Idempotent(Option<Claim>);

impl<S: Send + Sync> FromRequestParts<S> for Idempotent {
    type Rejection = Infallible;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        Ok(Self(parts.extensions.get::<Claim>().cloned()))
    }
}

impl Idempotent {
    /// Begins the transaction of the request's changes. With a key it locks the key's row until
    /// the transaction ends, and fails with `409 idempotency_key_in_use` once a repeat has taken
    /// the key over.
    pub(crate) async fn begin(
        &self,
        pool: &PgPool,
    ) -> Result<Transaction<'static, Postgres>, ApiError> {
        let mut transaction = pool.begin().await?;
        if let Some(claim) = &self.0 {
            let held = sqlx::query(
                "SELECT 1 FROM idempotency_keys \
                 WHERE account_id = $1 AND livemode = $2 AND key = $3 AND owner = $4 \
                   AND response IS NULL \
                 FOR UPDATE",
            )
            .bind(claim.scope.account_id())
            .bind(claim.scope.livemode())
            .bind(&claim.key)
            .bind(claim.owner)
            .fetch_optional(&mut *transaction)
            .await?
            .is_some();
            if !held {
                return Err(ApiError::idempotency_key_in_use());
            }
        }
        Ok(transaction)
    }

    /// Saves `response` as the key's result in `transaction`, begun by [`Self::begin`], and
    /// commits the transaction.
    pub(crate) async fn commit(
        &self,
        mut transaction: Transaction<'static, Postgres>,
        response: impl IntoResponse,
    ) -> Result<Response, ApiError> {
        let response = response.into_response();
        let Some(claim) = &self.0 else {
            transaction.commit().await?;
            return Ok(response);
        };
        let (mut parts, body) = response.into_parts();
        let bytes = to_bytes(body, MAX_BODY_BYTES).await.map_err(|error| {
            tracing::error!(%error, "response body could not be read for idempotency");
            ApiError::internal()
        })?;
        let stored = stored_response(
            parts.status,
            &bytes,
            parts.extensions.get::<ContainsSecret>().is_some(),
        )
        .ok_or_else(ApiError::internal)?;
        // The row is locked since `begin`, so the request still owns it.
        if !save(&mut *transaction, claim, &stored).await? {
            return Err(ApiError::idempotency_key_in_use());
        }
        transaction.commit().await?;
        parts.extensions.insert(Saved);
        Ok(Response::from_parts(parts, Body::from(bytes)))
    }
}

/// Serves a repeated merchant `POST` from `idempotency_keys`; runs after authentication and
/// authorization.
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
    let (mut parts, body) = request.into_parts();
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
    let claim = Claim {
        scope,
        key,
        owner: Uuid::new_v4(),
    };
    match acquire(&state.pool, &claim, &fingerprint).await {
        Ok(Acquired::Claimed) => {}
        Ok(Acquired::Replay(stored)) => return replay(&stored),
        Ok(Acquired::InUse) => return ApiError::idempotency_key_in_use().into_response(),
        Ok(Acquired::OtherRequest) => return ApiError::idempotency_key_reused().into_response(),
        Err(error) => return ApiError::from(error).into_response(),
    }
    parts.extensions.insert(claim.clone());

    let response = next.run(Request::from_parts(parts, Body::from(body))).await;
    if response.extensions().get::<Saved>().is_some() {
        return response;
    }
    if response.extensions().get::<NotExecuted>().is_some() {
        release(&state.pool, &claim).await;
        return response;
    }
    // A response not committed with the request's changes: its transaction rolled back, so the
    // response is the request's whole outcome. It is saved unless a repeat took the key over;
    // if saving fails, a repeat takes the key over after a minute and runs the request again.
    let (parts, body) = response.into_parts();
    let bytes = match to_bytes(body, MAX_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(error) => {
            tracing::error!(%error, "response body could not be read for idempotency");
            return ApiError::internal().into_response();
        }
    };
    let contains_secret = parts.extensions.get::<ContainsSecret>().is_some();
    if let Some(stored) = stored_response(parts.status, &bytes, contains_secret)
        && let Err(error) = save(&state.pool, &claim, &stored).await
    {
        tracing::error!(%error, "idempotent response was not stored");
    }
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

enum Acquired {
    /// This request holds the key and runs.
    Claimed,
    /// The stored response of the same request.
    Replay(Value),
    /// The same request is still running.
    InUse,
    /// The key was used with another request.
    OtherRequest,
}

async fn acquire(
    pool: &PgPool,
    claim: &Claim,
    fingerprint: &[u8; 32],
) -> Result<Acquired, sqlx::Error> {
    sqlx::query("DELETE FROM idempotency_keys WHERE created_at < now() - interval '24 hours'")
        .execute(pool)
        .await?;
    // A key whose request never saved a response is taken over by the same request after a
    // minute, under the new owner; an expired row not yet pruned by a concurrent request is
    // replaced. A takeover waits for the row lock of a transaction still committing, and then
    // finds its saved response.
    let claimed = sqlx::query(
        r#"
        INSERT INTO idempotency_keys (account_id, livemode, key, fingerprint, owner)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (account_id, livemode, key) DO UPDATE
            SET fingerprint = EXCLUDED.fingerprint, owner = EXCLUDED.owner, response = NULL,
                created_at = now()
            WHERE idempotency_keys.created_at < now() - interval '24 hours'
               OR (idempotency_keys.response IS NULL
                   AND idempotency_keys.fingerprint = EXCLUDED.fingerprint
                   AND idempotency_keys.created_at < now() - interval '1 minute')
        "#,
    )
    .bind(claim.scope.account_id())
    .bind(claim.scope.livemode())
    .bind(&claim.key)
    .bind(fingerprint.as_slice())
    .bind(claim.owner)
    .execute(pool)
    .await?
    .rows_affected()
        == 1;
    if claimed {
        return Ok(Acquired::Claimed);
    }
    let stored = sqlx::query_as::<_, (Vec<u8>, Option<Value>)>(
        "SELECT fingerprint, response FROM idempotency_keys \
         WHERE account_id = $1 AND livemode = $2 AND key = $3",
    )
    .bind(claim.scope.account_id())
    .bind(claim.scope.livemode())
    .bind(&claim.key)
    .fetch_optional(pool)
    .await?;
    Ok(match stored {
        // Released between the two statements: the client retries.
        None => Acquired::InUse,
        Some((stored, _)) if stored.as_slice() != fingerprint.as_slice() => Acquired::OtherRequest,
        Some((_, None)) => Acquired::InUse,
        Some((_, Some(response))) => Acquired::Replay(response),
    })
}

/// The stored form of a response, `{"status", "body"}`, without a `secret` when it contains one;
/// `None` for a body that is not JSON.
fn stored_response(status: StatusCode, body: &Bytes, contains_secret: bool) -> Option<Value> {
    let mut body = match serde_json::from_slice::<Value>(body) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(%error, "a merchant POST answered with a non-JSON body");
            return None;
        }
    };
    if contains_secret && let Some(object) = body.as_object_mut() {
        object.remove("secret");
    }
    Some(serde_json::json!({"status": status.as_u16(), "body": body}))
}

/// Saves `stored` as the key's response while `claim` still owns it; `false` once a repeat took
/// the key over or a response was saved.
async fn save<'e>(
    executor: impl PgExecutor<'e>,
    claim: &Claim,
    stored: &Value,
) -> Result<bool, sqlx::Error> {
    let saved = sqlx::query(
        "UPDATE idempotency_keys SET response = $5 \
         WHERE account_id = $1 AND livemode = $2 AND key = $3 AND owner = $4 \
           AND response IS NULL",
    )
    .bind(claim.scope.account_id())
    .bind(claim.scope.livemode())
    .bind(&claim.key)
    .bind(claim.owner)
    .bind(stored)
    .execute(executor)
    .await?
    .rows_affected();
    Ok(saved == 1)
}

/// Deletes the key's row, while `claim` still owns it and no response is saved, so a retry runs
/// the request again.
async fn release(pool: &PgPool, claim: &Claim) {
    let result = sqlx::query(
        "DELETE FROM idempotency_keys \
         WHERE account_id = $1 AND livemode = $2 AND key = $3 AND owner = $4 \
           AND response IS NULL",
    )
    .bind(claim.scope.account_id())
    .bind(claim.scope.livemode())
    .bind(&claim.key)
    .bind(claim.owner)
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
