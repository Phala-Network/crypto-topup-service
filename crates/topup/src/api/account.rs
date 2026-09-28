//! The account of the request's API key (`GET /v1/account`).

use axum::Json;
use axum::extract::{Extension, State};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::tenancy::{Permission, Scope};

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::models::AccountObject;

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
    Ok(row.map(
        |(id, name, charges_enabled, paused_scopes, created)| AccountObject {
            id,
            object: "account".to_owned(),
            livemode: scope.livemode(),
            name,
            charges_enabled,
            paused_scopes,
            created: created.timestamp(),
        },
    ))
}
