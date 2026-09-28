//! The merchant's API keys (`/v1/api_keys`, design D7): list, create, roll, and revoke the keys
//! of the requesting key's account and mode with a secret key. A test key never reaches live keys.

use axum::Json;
use axum::extract::{Extension, State};
use axum::response::{IntoResponse, Response};
use chrono::{Duration, Utc};

use crate::api_keys::{self, ApiKey, ApiKeyError, IssuedKey, KeyKind};
use crate::ids;
use crate::tenancy::Permission;

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath};
use super::idempotency::ContainsSecret;
use super::models::{ApiKeyList, ApiKeyObject, CreateApiKeyRequest, RollApiKeyRequest};

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    get,
    path = "/v1/api_keys",
    responses(
        (status = 200, description = "OK", body = ApiKeyList),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "api_keys"
)]
/// The keys of the requesting key's account and mode, newest first, without their secrets.
pub(crate) async fn list_api_keys(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
) -> ApiResult<Json<ApiKeyList>> {
    merchant
        .require(&state.pool, Permission::ApiKeysRead)
        .await?;
    let keys = api_keys::list(&state.pool, merchant.scope).await?;
    Ok(Json(ApiKeyList {
        object: "list".to_owned(),
        url: "/v1/api_keys".to_owned(),
        has_more: false,
        data: keys.iter().map(|key| api_key_object(key, None)).collect(),
    }))
}

#[utoipa::path(
    get,
    path = "/v1/api_keys/{id}",
    params(("id" = String, Path, description = "Key id, `key_…`")),
    responses(
        (status = 200, description = "OK", body = ApiKeyObject),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "api_keys"
)]
/// One key of the requesting key's account and mode, without its secret.
pub(crate) async fn get_api_key(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<ApiKeyObject>> {
    merchant
        .require(&state.pool, Permission::ApiKeysRead)
        .await?;
    let id = ids::parse(ids::API_KEY, &id).ok_or_else(ApiError::not_found)?;
    let key = api_keys::get(&state.pool, merchant.scope, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(api_key_object(&key, None)))
}

#[utoipa::path(
    post,
    path = "/v1/api_keys",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateApiKeyRequest,
    responses(
        (status = 200, description = "OK: the key with its `secret`, shown once", body = ApiKeyObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "Forbidden", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "api_keys"
)]
/// Creates a secret key in the requesting key's mode. The response is the only time its `secret`
/// is shown; a replay of the request (`Idempotency-Key`) returns the key without it.
pub(crate) async fn create_api_key(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateApiKeyRequest>,
) -> ApiResult<Response> {
    merchant
        .require(&state.pool, Permission::ApiKeysWrite)
        .await?;
    validate_name(&request.name)?;
    let issued = api_keys::create(
        &state.pool,
        merchant.scope,
        &request.name,
        &merchant.actor(),
        "",
    )
    .await
    .map_err(map_error)?;
    Ok(issued_response(&issued))
}

#[utoipa::path(
    post,
    path = "/v1/api_keys/{id}/roll",
    params(
        ("id" = String, Path, description = "Key id, `key_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = RollApiKeyRequest,
    responses(
        (status = 200, description = "OK: the new key with its `secret`, shown once", body = ApiKeyObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (status = 409, description = "`api_key_inactive`: revoked or already rolled", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "api_keys"
)]
/// Rolls a key: returns a new secret key with the same name, and the old key keeps working for
/// `expires_in` seconds (at most 7 days), Stripe's roll; `0`, the default, revokes it at once. A
/// key may roll itself.
pub(crate) async fn roll_api_key(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
    ApiJson(request): ApiJson<RollApiKeyRequest>,
) -> ApiResult<Response> {
    merchant
        .require(&state.pool, Permission::ApiKeysWrite)
        .await?;
    let id = ids::parse(ids::API_KEY, &id).ok_or_else(ApiError::not_found)?;
    let issued = api_keys::roll(
        &state.pool,
        merchant.scope,
        id,
        Duration::seconds(i64::from(request.expires_in)),
        &merchant.actor(),
    )
    .await
    .map_err(map_error)?;
    Ok(issued_response(&issued))
}

#[utoipa::path(
    delete,
    path = "/v1/api_keys/{id}",
    params(("id" = String, Path, description = "Key id, `key_…`")),
    responses(
        (status = 200, description = "OK: revoked, or already revoked", body = ApiKeyObject),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (
            status = 409,
            description = "`last_api_key`: the mode's last key that is neither revoked nor \
                           expiring; create or roll a key first",
            body = ErrorResponse
        )
    ),
    security(("api_key" = [])),
    tag = "api_keys"
)]
/// Revokes a key at once. The mode's last key that is neither revoked nor expiring cannot be
/// revoked, so the account always keeps a working key; to replace a leaked last key, roll it with
/// `expires_in: 0`.
pub(crate) async fn revoke_api_key(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<ApiKeyObject>> {
    merchant
        .require(&state.pool, Permission::ApiKeysWrite)
        .await?;
    let id = ids::parse(ids::API_KEY, &id).ok_or_else(ApiError::not_found)?;
    let key = api_keys::revoke(&state.pool, merchant.scope, id, &merchant.actor())
        .await
        .map_err(map_error)?;
    Ok(Json(api_key_object(&key, None)))
}

/// A newly issued key's response: shown with its secret, which the idempotency layer never
/// stores.
pub(crate) fn issued_response(issued: &IssuedKey) -> Response {
    (
        Extension(ContainsSecret),
        Json(api_key_object(
            &issued.key,
            Some(issued.secret.as_str().to_owned()),
        )),
    )
        .into_response()
}

/// The API representation of `key`.
pub(crate) fn api_key_object(key: &ApiKey, secret: Option<String>) -> ApiKeyObject {
    let now = Utc::now();
    let status = if key.revoked_at.is_some() {
        "revoked"
    } else {
        match key.expires_at {
            None => "active",
            Some(expires_at) if expires_at > now => "expiring",
            Some(_) => "expired",
        }
    };
    ApiKeyObject {
        id: key.public_id(),
        object: "api_key".to_owned(),
        livemode: key.livemode,
        key_type: match key.kind {
            KeyKind::Secret => "secret",
            KeyKind::Restricted => "restricted",
        }
        .to_owned(),
        name: key.name.clone(),
        secret,
        redacted: format!("{}…{}", key.prefix, key.last4),
        status: status.to_owned(),
        created: key.created_at.timestamp(),
        expires_at: key.expires_at.map(|time| time.timestamp()),
        last_used: key.last_used_at.map(|time| time.timestamp()),
    }
}

/// A key's label is at most 200 characters.
pub(crate) fn validate_name(name: &str) -> ApiResult<()> {
    if name.chars().count() > 200 {
        return Err(ApiError::invalid_param(
            "name",
            "name must be at most 200 characters",
        ));
    }
    Ok(())
}

/// Maps a key repository failure to its API error.
pub(crate) fn map_error(error: ApiKeyError) -> ApiError {
    match error {
        ApiKeyError::NotFound => ApiError::not_found(),
        ApiKeyError::Inactive => ApiError::api_key_inactive(),
        ApiKeyError::LastActiveKey => ApiError::last_api_key(),
        ApiKeyError::ChargesNotEnabled => ApiError::testmode_charges_only(),
        ApiKeyError::InvalidExpiry => ApiError::invalid_param(
            "expires_in",
            "expires_in must be between 0 and 604800 seconds (7 days)",
        ),
        ApiKeyError::EntropyUnavailable => ApiError::internal(),
        ApiKeyError::Database(error) => error.into(),
    }
}
