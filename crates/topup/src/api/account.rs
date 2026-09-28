//! The account of the request's API key (`GET /v1/account`), its webhook keys (design D11), and
//! the attestation that binds them (`GET /v1/attestation`).

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, State};
use chrono::{DateTime, Duration, Utc};
use sqlx::PgPool;
use topup_core::route::Confirmations;
use uuid::Uuid;

use crate::audit::Actor;
use crate::pause::PauseOwner;
use crate::routes::RouteSet;
use crate::tenancy::{Permission, Scope};
use crate::webhook_keys::{self, WebhookKeyError, WebhookKeys};

use super::AppState;
use super::attestation::{AttestationError, AttestationRequest};
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiQuery};
use super::models::{
    AccountObject, AccountSelfPauseRequest, AttestationQuery, AttestationResponse,
    ConfirmationPolicy, RollWebhookKeyRequest, UpdateAccountObjectRequest, WebhookKeyObject,
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
    current_account(&state, merchant.scope).await
}

#[utoipa::path(
    post,
    path = "/v1/account",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = UpdateAccountObjectRequest,
    responses(
        (status = 200, description = "OK", body = AccountObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "account"
)]
/// Updates the account's settings in the key's mode; parameters not sent are left unchanged.
/// `confirmation_policies` sets, per chain, the confirmation a payment must reach before it is
/// credited (design D1): the stricter of it and the route's floor applies to every deposit not
/// credited yet, and `GET /v1/config` reports it with its typical credit time. Announced as
/// `account.updated`.
pub(crate) async fn update_account(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<UpdateAccountObjectRequest>,
) -> Result<Json<AccountObject>, ApiError> {
    merchant
        .require(&state.pool, Permission::AccountWrite)
        .await?;
    if let Some(policies) = request.confirmation_policies {
        let changes = validate_policies(&state.routes, merchant.scope, &policies)?;
        set_policies(
            &state.pool,
            &state.routes,
            merchant.scope,
            &changes,
            &merchant.actor(),
        )
        .await?;
    }
    current_account(&state, merchant.scope).await
}

#[utoipa::path(
    post,
    path = "/v1/account/pause",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = AccountSelfPauseRequest,
    responses(
        (status = 200, description = "OK", body = AccountObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "account"
)]
/// Pauses the account's `quotes` in both modes, for an emergency such as a leaked key during a
/// treasury time-lock (design §12): no quote, deposit address, or network is issued until you
/// resume. Payments to existing addresses keep being credited. Announced as `account.updated`.
pub(crate) async fn pause_account(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<AccountSelfPauseRequest>,
) -> Result<Json<AccountObject>, ApiError> {
    self_pause(&state, &merchant, &request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/account/resume",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = AccountSelfPauseRequest,
    responses(
        (status = 200, description = "OK", body = AccountObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "account"
)]
/// Resumes the `quotes` you paused. A pause the operator set stays in `paused_scopes` until the
/// operator lifts it. Announced as `account.updated`.
pub(crate) async fn resume_account(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<AccountSelfPauseRequest>,
) -> Result<Json<AccountObject>, ApiError> {
    self_pause(&state, &merchant, &request, false).await
}

async fn self_pause(
    state: &AppState,
    merchant: &Merchant,
    request: &AccountSelfPauseRequest,
    pause: bool,
) -> Result<Json<AccountObject>, ApiError> {
    merchant
        .require(&state.pool, Permission::AccountWrite)
        .await?;
    if request.scopes.is_empty() || request.scopes.iter().any(|scope| scope != "quotes") {
        return Err(ApiError::invalid_param(
            "scopes",
            "scopes must be [\"quotes\"], the one scope an account pauses itself",
        ));
    }
    let mut transaction = state.pool.begin().await?;
    crate::pause::mutate_account_scopes_in(
        &mut transaction,
        &state.routes,
        merchant.scope.account_id(),
        PauseOwner::Merchant,
        &["quotes"],
        pause,
        &merchant.actor(),
        if pause {
            "paused through the API"
        } else {
            "resumed through the API"
        },
    )
    .await?
    .ok_or_else(ApiError::internal)?;
    transaction.commit().await?;
    current_account(state, merchant.scope).await
}

/// Checks each policy against its chain's route floor: a chain of the key's mode, a value of
/// the chain's kind, and never weaker than the route's. `None` removes a chain's policy.
fn validate_policies(
    routes: &RouteSet,
    scope: Scope,
    policies: &[ConfirmationPolicy],
) -> Result<BTreeMap<u64, Option<Confirmations>>, ApiError> {
    let mut changes = BTreeMap::new();
    for (index, policy) in policies.iter().enumerate() {
        let param = |field: &str| format!("confirmation_policies[{index}][{field}]");
        let floor = routes
            .current_in(scope.livemode())
            .find(|route| route.chain.chain_id == policy.chain_id)
            .map(|route| route.chain.confirmations)
            .ok_or_else(|| {
                ApiError::invalid_param(param("chain_id"), "not a chain of the key's mode")
            })?;
        let value = policy
            .confirmations
            .as_deref()
            .map(|value| {
                let required = Confirmations::parse_policy(value).ok_or_else(|| {
                    ApiError::invalid_param(
                        param("confirmations"),
                        "confirmations must be a depth of 1 to 999999, safe, or finalized",
                    )
                })?;
                if floor.stricter(required) == Some(required) {
                    Ok(required)
                } else {
                    Err(ApiError::invalid_param(
                        param("confirmations"),
                        format!(
                            "the chain's route requires {}; a policy may only be stricter, of \
                             the same kind or finalized",
                            floor.policy_value()
                        ),
                    ))
                }
            })
            .transpose()?;
        if changes.insert(policy.chain_id, value).is_some() {
            return Err(ApiError::invalid_param(
                param("chain_id"),
                "each chain may be listed once",
            ));
        }
    }
    Ok(changes)
}

/// Writes the policy changes with an audit row and, when any changed, `account.updated` in the
/// key's mode, whose chains they are.
async fn set_policies(
    pool: &PgPool,
    routes: &RouteSet,
    scope: Scope,
    changes: &BTreeMap<u64, Option<Confirmations>>,
    actor: &Actor,
) -> Result<(), ApiError> {
    let mut transaction = pool.begin().await?;
    let object = crate::db::EventObject::Account(scope.account_id());
    let before = crate::db::render(&mut transaction, routes, scope, object).await?;
    let mut changed = false;
    for (&chain_id, value) in changes {
        let chain_id = i64::try_from(chain_id).map_err(|_| ApiError::internal())?;
        let affected = match value {
            Some(required) => sqlx::query(
                "INSERT INTO confirmation_policies (account_id, chain_id, required) \
                 VALUES ($1, $2, $3) ON CONFLICT (account_id, chain_id) DO UPDATE \
                 SET required = EXCLUDED.required \
                 WHERE confirmation_policies.required <> EXCLUDED.required",
            )
            .bind(scope.account_id())
            .bind(chain_id)
            .bind(required.policy_value())
            .execute(&mut *transaction)
            .await?
            .rows_affected(),
            None => sqlx::query(
                "DELETE FROM confirmation_policies WHERE account_id = $1 AND chain_id = $2",
            )
            .bind(scope.account_id())
            .bind(chain_id)
            .execute(&mut *transaction)
            .await?
            .rows_affected(),
        };
        changed |= affected > 0;
    }
    if changed {
        let public_id: String = sqlx::query_scalar("SELECT public_id FROM accounts WHERE id = $1")
            .bind(scope.account_id())
            .fetch_one(&mut *transaction)
            .await?;
        crate::audit::insert(
            &mut *transaction,
            &crate::audit::Entry {
                account_id: Some(scope.account_id()),
                actor,
                action: "account.confirmation_policies",
                subject: &format!("account:{public_id}"),
                reason: &serde_json::json!(
                    changes
                        .iter()
                        .map(|(chain, value)| (
                            chain.to_string(),
                            value.map(Confirmations::policy_value)
                        ))
                        .collect::<BTreeMap<_, _>>()
                )
                .to_string(),
            },
        )
        .await?;
        let event = crate::db::NewOutboxEvent::new("account.updated", scope, object, actor);
        crate::db::enqueue_in(&mut transaction, routes, &event, Some(&before)).await?;
    }
    transaction.commit().await?;
    Ok(())
}

/// The confirmations the account requires, by chain, across both modes.
pub(crate) async fn confirmation_policies<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    account_id: Uuid,
) -> Result<BTreeMap<u64, Confirmations>, ApiError> {
    let rows: Vec<(i64, String)> = sqlx::query_as(
        "SELECT chain_id, required FROM confirmation_policies WHERE account_id = $1",
    )
    .bind(account_id)
    .fetch_all(executor)
    .await?;
    rows.into_iter()
        .map(|(chain_id, required)| {
            Ok((
                u64::try_from(chain_id).map_err(|_| ApiError::internal())?,
                Confirmations::parse_policy(&required).ok_or_else(ApiError::internal)?,
            ))
        })
        .collect()
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
/// now on, and the current one keeps signing beside it for `expires_in` seconds, so every
/// delivery carries one `v1a` signature per key until then. The overlap is 48 hours (the default,
/// the treasury time-lock) to 7 days in live mode, so a leaked key cannot cut off the key you
/// pinned; test mode also accepts `0`, which stops it at once. The roll is announced as
/// `account.updated`, signed by the retiring key as well even after its overlap. Fetch and verify
/// the new public key with `GET /v1/attestation`, pin it next to the old one, and drop the old one
/// when it expires.
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
        &state.routes,
        merchant.scope,
        Duration::seconds(i64::from(request.expires_in)),
        &merchant.actor(),
    )
    .await
    .map_err(|error| match error {
        WebhookKeyError::InvalidExpiry => {
            let (min, max) = webhook_keys::overlap_range(merchant.scope.livemode());
            ApiError::invalid_param(
                "expires_in",
                format!(
                    "expires_in must be between {} and {} seconds in this mode",
                    min.num_seconds(),
                    max.num_seconds()
                ),
            )
        }
        WebhookKeyError::NotFound | WebhookKeyError::VersionExhausted => ApiError::internal(),
        WebhookKeyError::Database(error) => error.into(),
    })?;
    current_account(&state, merchant.scope).await
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
        tdx_quote: hex::encode(evidence.quote),
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

/// The scope's account as `GET /v1/account` returns it.
async fn current_account(state: &AppState, scope: Scope) -> Result<Json<AccountObject>, ApiError> {
    let mut connection = state.pool.acquire().await?;
    find_account(&mut connection, &state.routes, scope)
        .await?
        .map(Json)
        .ok_or_else(ApiError::internal)
}

/// The API representation of the scope's account: its policies of the chains of the scope's
/// mode, and the operator's and its own pauses.
pub(crate) async fn find_account(
    connection: &mut sqlx::PgConnection,
    routes: &RouteSet,
    scope: Scope,
) -> Result<Option<AccountObject>, ApiError> {
    let row = sqlx::query_as::<_, (String, String, bool, Vec<String>, DateTime<Utc>)>(
        "SELECT public_id, name, charges_enabled, \
                ARRAY(SELECT DISTINCT scope FROM unnest(paused_scopes || self_paused_scopes) \
                      AS scope ORDER BY scope), \
                created_at \
         FROM accounts WHERE id = $1",
    )
    .bind(scope.account_id())
    .fetch_optional(&mut *connection)
    .await?;
    let Some((id, name, charges_enabled, paused_scopes, created)) = row else {
        return Ok(None);
    };
    let webhook_keys = webhook_keys::active(&mut *connection, scope)
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
    let chains = routes
        .current_in(scope.livemode())
        .map(|route| route.chain.chain_id)
        .collect::<std::collections::BTreeSet<_>>();
    let confirmation_policies = confirmation_policies(&mut *connection, scope.account_id())
        .await?
        .into_iter()
        .filter(|(chain_id, _)| chains.contains(chain_id))
        .map(|(chain_id, required)| ConfirmationPolicy {
            chain_id,
            confirmations: Some(required.policy_value()),
        })
        .collect();
    Ok(Some(AccountObject {
        id,
        object: "account".to_owned(),
        livemode: scope.livemode(),
        name,
        charges_enabled,
        paused_scopes,
        webhook_keys,
        confirmation_policies,
        created: created.timestamp(),
    }))
}
