//! The account of the request's API key (`GET /v1/account`), its webhook keys (design D11), and
//! the attestation that binds them (`GET /v1/attestation`).

use axum::Json;
use axum::extract::{Extension, State};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;

use crate::tenancy::{Permission, Scope};
use crate::webhook_keys::{self, WebhookKeyError, WebhookKeys};

use super::AppState;
use super::attestation::{AttestationError, AttestationRequest};
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiQuery};
use super::models::{
    AccountObject, AttestationQuery, AttestationResponse, RollWebhookKeyRequest, WebhookKeyObject,
    WebhookKeyVersion,
};

#[utoipa::path(
    get,
    path = "/v1/account",
    responses(
        (status = 200, description = "OK", body = AccountObject),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "account"
)]
/// The account the API key belongs to, in the key's mode.
pub(crate) async fn get_account(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
) -> Result<Json<AccountObject>, ApiError> {
    merchant
        .require(&state.pool, Permission::AccountRead)
        .await?;
    find_account(&state.pool, merchant.scope)
        .await?
        .map(Json)
        .ok_or_else(ApiError::internal)
}

#[utoipa::path(
    post,
    path = "/v1/account/webhook_keys/roll",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters. A retry with the same key and request within 24 hours \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = RollWebhookKeyRequest,
    responses(
        (status = 200, description = "OK: the account with its new webhook key version", body = AccountObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "account"
)]
/// Rolls the webhook signing key of the key's mode: the next version signs every delivery from
/// now on, and the current one keeps signing beside it for `expires_in` seconds (at most 7 days),
/// so every delivery carries one `v1a` signature per key until then; `0`, the default, stops it
/// at once. Fetch and verify the new public key with `GET /v1/attestation`, pin it next to the
/// old one, and drop the old one when it expires. Announced as `account.updated`.
pub(crate) async fn roll_webhook_key(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<RollWebhookKeyRequest>,
) -> Result<Json<AccountObject>, ApiError> {
    merchant
        .require(&state.pool, Permission::AccountWrite)
        .await?;
    webhook_keys::roll(
        &state.pool,
        merchant.scope,
        Duration::seconds(i64::from(request.expires_in)),
        &merchant.actor(),
    )
    .await
    .map_err(|error| match error {
        WebhookKeyError::InvalidExpiry => ApiError::invalid_param(
            "expires_in",
            "expires_in must be between 0 and 604800 seconds (7 days)",
        ),
        WebhookKeyError::NotFound | WebhookKeyError::VersionExhausted => ApiError::internal(),
        WebhookKeyError::Database(error) => error.into(),
    })?;
    find_account(&state.pool, merchant.scope)
        .await?
        .map(Json)
        .ok_or_else(ApiError::internal)
}

#[utoipa::path(
    get,
    path = "/v1/attestation",
    params(AttestationQuery),
    responses(
        (status = 200, description = "OK", body = AttestationResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 503, description = "Service Unavailable", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "attestation"
)]
/// TDX evidence binding a fresh `nonce` to the webhook public keys of the key's account and mode
/// (design D11). Verify the quote once with the dstack verifier, check that its report data is
/// `report_data` zero-padded to 64 bytes and that `report_data` binds your nonce, account, mode,
/// and the listed keys, then pin the public keys: they are stable across releases.
pub(crate) async fn get_attestation(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiQuery(query): ApiQuery<AttestationQuery>,
) -> Result<Json<AttestationResponse>, ApiError> {
    merchant
        .require(&state.pool, Permission::AccountRead)
        .await?;
    let nonce = decode_nonce(&query.nonce)?;
    let keys = active_keys(&state.pool, merchant.scope)
        .await?
        .ok_or_else(ApiError::internal)?;
    let versions: Vec<u32> = keys.versions.iter().map(|key| key.version).collect();
    let evidence = state
        .attestor
        .attest(AttestationRequest {
            nonce: &nonce,
            account: &keys.account,
            livemode: keys.livemode,
            versions: &versions,
        })
        .await
        .map_err(|AttestationError::Unavailable| {
            ApiError::service_unavailable("attestation is unavailable")
        })?;
    let attested: Vec<u32> = evidence
        .webhook_keys
        .iter()
        .map(|key| key.version)
        .collect();
    if attested != versions {
        tracing::error!("the attestor returned other webhook key versions than requested");
        return Err(ApiError::internal());
    }
    Ok(Json(AttestationResponse {
        object: "attestation".to_owned(),
        account: keys.account,
        livemode: keys.livemode,
        webhook_keys: evidence
            .webhook_keys
            .iter()
            .zip(&keys.versions)
            .map(|(key, version)| WebhookKeyObject {
                version: key.version,
                public_key: hex::encode(key.public_key.0),
                expires_at: version.expires_at.map(|time| time.timestamp()),
            })
            .collect(),
        report_data: hex::encode(evidence.report_data),
        quote: hex::encode(evidence.quote),
    }))
}

fn decode_nonce(value: &str) -> Result<Vec<u8>, ApiError> {
    if value.is_empty() || value.len() > 64 {
        return Err(ApiError::bad_request(
            "nonce must be 1 to 32 bytes of hexadecimal",
        ));
    }
    hex::decode(value).map_err(|_| ApiError::bad_request("nonce must be valid hexadecimal"))
}

async fn active_keys(pool: &PgPool, scope: Scope) -> Result<Option<WebhookKeys>, sqlx::Error> {
    let mut connection = pool.acquire().await?;
    webhook_keys::active(&mut connection, scope).await
}

/// The API representation of the scope's account.
pub(crate) async fn find_account(
    pool: &PgPool,
    scope: Scope,
) -> Result<Option<AccountObject>, sqlx::Error> {
    let row = sqlx::query_as::<_, (String, String, bool, Vec<String>, DateTime<Utc>)>(
        "SELECT public_id, name, charges_enabled, paused_scopes, created_at \
         FROM accounts WHERE id = $1",
    )
    .bind(scope.account_id())
    .fetch_optional(pool)
    .await?;
    let Some((id, name, charges_enabled, paused_scopes, created)) = row else {
        return Ok(None);
    };
    let webhook_keys = active_keys(pool, scope)
        .await?
        .map(|keys| {
            keys.versions
                .iter()
                .map(|key| WebhookKeyVersion {
                    version: key.version,
                    expires_at: key.expires_at.map(|time| time.timestamp()),
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(AccountObject {
        id,
        object: "account".to_owned(),
        livemode: scope.livemode(),
        name,
        charges_enabled,
        paused_scopes,
        webhook_keys,
        created: created.timestamp(),
    }))
}
