//! Typed API handlers for the routes in architecture §12.

use std::str::FromStr;

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::B256;
use axum::Json;
use axum::extract::State;
use topup_core::screening::PauseScope;
use uuid::Uuid;

use crate::audit::Actor;
use crate::db::Customer;
use crate::routes::{ProviderError, RouteSet};
use crate::tenancy::Scope;

use super::AppState;
use super::attestation::AttestationError;
use super::auth::VerificationKey;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, ApiQuery};
use super::models::{
    AccountResponse, AdminReasonRequest, AdminRefundResponse, AttestationQuery,
    AttestationResponse, CreateAccountRequest, DailyReportResponse, NudgeResponse,
    OutboxReplayResponse, PauseRequest, PauseResponse, ReconciliationBlockLiftResponse,
    RecordRefundRequest, RoutePauseResponse, SupportDepositResponse, UpdateAccountRequest,
};
use super::repository::{self, IssuedAccount};

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    get,
    path = "/v1/attestation",
    params(AttestationQuery),
    responses(
        (status = 200, description = "OK", body = AttestationResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 503, description = "Service Unavailable", body = ErrorResponse)
    ),
    tag = "attestation"
)]
pub(crate) async fn get_attestation(
    State(state): State<AppState>,
    ApiQuery(query): ApiQuery<AttestationQuery>,
) -> ApiResult<Json<AttestationResponse>> {
    let nonce = decode_nonce(&query.nonce)?;
    match state.attestor.attest(&nonce).await {
        Ok(response) => Ok(Json(response)),
        Err(AttestationError::Unavailable) => {
            Err(ApiError::service_unavailable("attestation is unavailable"))
        }
    }
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 200, description = "OK: issued", body = AccountResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Issues a merchant account with its request signing key (key id `{id}/v1`) and webhook URL.
/// Each call issues a new account. Until self-serve signup and API keys replace it.
pub(crate) async fn create_account(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateAccountRequest>,
) -> ApiResult<Json<AccountResponse>> {
    let name = request.name.trim();
    if name.is_empty() || name.chars().count() > 200 {
        return Err(ApiError::invalid_param(
            "name",
            "name must contain 1 to 200 characters",
        ));
    }
    validate_credentials(&state, &request.public_key, &request.webhook_url)?;
    let account = repository::create_account(
        &state.pool,
        name,
        request.livemode,
        &request.public_key,
        &request.webhook_url,
        &admin_actor(&state),
    )
    .await?;
    Ok(Json(account_response(account)))
}

#[utoipa::path(
    put,
    path = "/v1/admin/accounts/{account}",
    params(("account" = String, Path, description = "Account id, `acct_…`")),
    request_body = UpdateAccountRequest,
    responses(
        (status = 200, description = "OK: replaced, or already holding these values", body = AccountResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found: no account is issued with this id", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Replaces an account's request signing key and webhook URL; the key id and mode stay.
pub(crate) async fn update_account(
    State(state): State<AppState>,
    ApiPath(account): ApiPath<String>,
    ApiJson(request): ApiJson<UpdateAccountRequest>,
) -> ApiResult<Json<AccountResponse>> {
    let account_id = parse_account_id(&account)?;
    validate_credentials(&state, &request.public_key, &request.webhook_url)?;
    validate_reason(&request.reason)?;
    let account = repository::update_account(
        &state.pool,
        account_id,
        &request.public_key,
        &request.webhook_url,
        &admin_actor(&state),
        &request.reason,
    )
    .await?;
    Ok(Json(account_response(account)))
}

#[utoipa::path(
    get,
    path = "/v1/admin/deposits/{id}",
    params(("id" = String, Path, description = "Deposit id, `dep_…` or the UUID")),
    responses(
        (status = 200, description = "OK", body = SupportDepositResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// One deposit of any account with its stored facts, transitions, and webhook events.
pub(crate) async fn admin_get_deposit(
    State(state): State<AppState>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<SupportDepositResponse>> {
    let id = crate::ids::parse_or_uuid(crate::ids::DEPOSIT, &id).ok_or_else(ApiError::not_found)?;
    repository::admin_deposit(&state.pool, id)
        .await?
        .map(Json)
        .ok_or_else(ApiError::not_found)
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}/customers/{customer}/pause",
    params(
        ("account" = String, Path, description = "Account id, `acct_…`"),
        ("customer" = String, Path, description = "The account's identifier of its customer, the quotes' `account_id`")
    ),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.
pub(crate) async fn pause_customer(
    State(state): State<AppState>,
    ApiPath((account, customer)): ApiPath<(String, String)>,
    ApiJson(request): ApiJson<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_customer_scopes(&state, &account, &customer, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}/customers/{customer}/resume",
    params(
        ("account" = String, Path, description = "Account id, `acct_…`"),
        ("customer" = String, Path, description = "The account's identifier of its customer, the quotes' `account_id`")
    ),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Resumes scopes of one customer of an account.
pub(crate) async fn resume_customer(
    State(state): State<AppState>,
    ApiPath((account, customer)): ApiPath<(String, String)>,
    ApiJson(request): ApiJson<PauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_customer_scopes(&state, &account, &customer, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/routes/{r}/pause",
    params(("r" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = RoutePauseResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn pause_route(
    State(state): State<AppState>,
    ApiPath(route): ApiPath<String>,
    ApiJson(request): ApiJson<PauseRequest>,
) -> ApiResult<Json<RoutePauseResponse>> {
    mutate_route_scopes(&state, &route, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/routes/{r}/resume",
    params(("r" = String, Path)),
    request_body = PauseRequest,
    responses((status = 200, description = "OK", body = RoutePauseResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn resume_route(
    State(state): State<AppState>,
    ApiPath(route): ApiPath<String>,
    ApiJson(request): ApiJson<PauseRequest>,
) -> ApiResult<Json<RoutePauseResponse>> {
    mutate_route_scopes(&state, &route, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/deposits/{id}/nudge",
    params(("id" = String, Path, description = "Deposit id, `dep_…` or the UUID")),
    responses(
        (status = 200, description = "OK", body = NudgeResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn nudge_deposit(
    State(state): State<AppState>,
    ApiPath(deposit_id): ApiPath<String>,
) -> ApiResult<Json<NudgeResponse>> {
    let deposit_id = crate::ids::parse_or_uuid(crate::ids::DEPOSIT, &deposit_id)
        .ok_or_else(ApiError::not_found)?;
    Ok(Json(
        repository::nudge_deposit(&state.pool, deposit_id, &admin_actor(&state)).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/approve",
    params(("id" = String, Path, description = "Refund id, `re_…` or the UUID")),
    responses(
        (status = 200, description = "OK", body = AdminRefundResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn approve_refund(
    State(state): State<AppState>,
    ApiPath(refund_id): ApiPath<String>,
) -> ApiResult<Json<AdminRefundResponse>> {
    let refund_id = parse_refund_id(&refund_id)?;
    Ok(Json(
        repository::approve_refund(
            &state.pool,
            refund_id,
            state.routes.routes(),
            &admin_actor(&state),
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/refunds/{id}/record",
    params(("id" = String, Path, description = "Refund id, `re_…` or the UUID")),
    request_body = RecordRefundRequest,
    responses(
        (status = 200, description = "OK", body = AdminRefundResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (status = 409, description = "Conflict", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn record_refund(
    State(state): State<AppState>,
    ApiPath(refund_id): ApiPath<String>,
    ApiJson(request): ApiJson<RecordRefundRequest>,
) -> ApiResult<Json<AdminRefundResponse>> {
    let refund_id = parse_refund_id(&refund_id)?;
    let tx_hash = B256::from_str(&request.tx_hash)
        .map_err(|_| ApiError::bad_request("tx_hash must be a 32-byte hexadecimal value"))?;
    Ok(Json(
        repository::record_refund(&state.pool, refund_id, tx_hash, &admin_actor(&state)).await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/reconciliation-blocks/{block_key}/lift",
    params(("block_key" = String, Path, description = "`chain:{chain_id}` or `address:{address_id}`, as listed in the daily report")),
    request_body = AdminReasonRequest,
    responses(
        (status = 200, description = "OK: lifted, or already lifted", body = ReconciliationBlockLiftResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found: no active or lifted block has this key", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn lift_reconciliation_block(
    State(state): State<AppState>,
    ApiPath(block_key): ApiPath<String>,
    ApiJson(request): ApiJson<AdminReasonRequest>,
) -> ApiResult<Json<ReconciliationBlockLiftResponse>> {
    validate_reason(&request.reason)?;
    Ok(Json(
        repository::lift_reconciliation_block(
            &state.pool,
            &block_key,
            &admin_actor(&state),
            &request.reason,
        )
        .await?,
    ))
}

#[utoipa::path(
    post,
    path = "/v1/admin/outbox/{event_id}/replay",
    params((
        "event_id" = String,
        Path,
        description = "The `webhook-id` header, `evt_…`"
    )),
    request_body = AdminReasonRequest,
    responses(
        (status = 200, description = "OK: queued for delivery", body = OutboxReplayResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn replay_outbox_event(
    State(state): State<AppState>,
    ApiPath(event_id): ApiPath<String>,
    ApiJson(request): ApiJson<AdminReasonRequest>,
) -> ApiResult<Json<OutboxReplayResponse>> {
    let event_id =
        crate::ids::parse(crate::ids::EVENT, &event_id).ok_or_else(ApiError::not_found)?;
    validate_reason(&request.reason)?;
    Ok(Json(
        repository::replay_outbox_event(
            &state.pool,
            event_id,
            &admin_actor(&state),
            &request.reason,
        )
        .await?,
    ))
}

#[utoipa::path(
    get,
    path = "/v1/admin/report/daily",
    responses((status = 200, description = "OK", body = DailyReportResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn daily_report(
    State(state): State<AppState>,
) -> ApiResult<Json<DailyReportResponse>> {
    let mut report =
        repository::daily_report(&state.pool, state.routes.routes(), chrono::Utc::now()).await?;
    populate_treasury_balances(&state.routes, &mut report).await;
    report.reconciliation = crate::observability::reconciliation().map(Into::into);
    Ok(Json(report))
}

async fn populate_treasury_balances(routes: &RouteSet, report: &mut DailyReportResponse) {
    for route_report in &mut report.routes {
        let Some(route) = routes
            .routes()
            .iter()
            .filter(|route| route.route == route_report.route)
            .max_by_key(|route| route.version)
        else {
            continue;
        };
        let chain_id = route.chain.chain_id;
        let client = match routes.provider(chain_id, 0) {
            Ok(client) => client,
            Err(ProviderError::MissingUrl { environment, .. }) => {
                route_report.treasury_balance_note =
                    format!("treasury balance unavailable: {environment} is not configured");
                continue;
            }
            Err(ProviderError::Unconfigured { .. }) => {
                route_report.treasury_balance_note =
                    "treasury balance unavailable: route has no RPC provider".to_owned();
                continue;
            }
            Err(ProviderError::InvalidKey { environment, .. }) => {
                route_report.treasury_balance_note =
                    format!("treasury balance unavailable: {environment} does not fit its URL");
                continue;
            }
            Err(ProviderError::InvalidUrl { .. }) => {
                route_report.treasury_balance_note =
                    "treasury balance unavailable: RPC client configuration is invalid".to_owned();
                continue;
            }
        };
        match client
            .token_balances(
                route.asset.contract,
                &[route.chain.contracts.treasury],
                BlockNumberOrTag::Latest,
            )
            .await
        {
            Ok(balances) => match balances.into_iter().next() {
                Some(balance) => {
                    route_report.treasury_balance_atomic = Some(balance.to_string());
                    route_report.treasury_balance_note =
                        "latest on-chain ERC-20 treasury balance".to_owned();
                }
                None => {
                    route_report.treasury_balance_note =
                        "treasury balance unavailable: RPC returned no balance".to_owned();
                }
            },
            Err(error) => {
                tracing::warn!(route = %route.route, %error, "daily report treasury balance read failed");
                route_report.treasury_balance_note =
                    "treasury balance unavailable: RPC read failed".to_owned();
            }
        }
    }
}

/// Finds or creates the scope's customer, so creating a quote is one call.
pub(super) async fn ensure_customer(
    state: &AppState,
    scope: Scope,
    client_reference_id: &str,
) -> ApiResult<Customer> {
    validate_external_id(client_reference_id)?;
    repository::ensure_customer(&state.pool, scope, client_reference_id).await
}

async fn mutate_customer_scopes(
    state: &AppState,
    account: &str,
    client_reference_id: &str,
    request: PauseRequest,
    pause: bool,
) -> ApiResult<Json<PauseResponse>> {
    let scopes = validate_scopes(request.scopes)?;
    let account_id = parse_account_id(account)?;
    let livemode = repository::find_signing_key(&state.pool, account_id)
        .await?
        .ok_or_else(ApiError::not_found)?
        .livemode;
    // The operator names the customer in the mode of the account's signing key.
    let customer = repository::find_customer(
        &state.pool,
        Scope::new(account_id, livemode),
        client_reference_id,
    )
    .await?
    .ok_or_else(ApiError::not_found)?;
    let updated = repository::mutate_customer_scopes(
        &state.pool,
        &customer,
        &scopes,
        pause,
        &admin_actor(state),
    )
    .await?;
    Ok(Json(PauseResponse {
        paused_scopes: updated,
    }))
}

async fn mutate_route_scopes(
    state: &AppState,
    route: &str,
    request: PauseRequest,
    pause: bool,
) -> ApiResult<Json<RoutePauseResponse>> {
    if !state
        .routes
        .routes()
        .iter()
        .any(|candidate| candidate.route == route)
    {
        return Err(ApiError::not_found());
    }
    let scopes = validate_scopes(request.scopes)?;
    let updated =
        repository::mutate_route_scopes(&state.pool, route, &scopes, pause, &admin_actor(state))
            .await?;
    Ok(Json(RoutePauseResponse {
        route: route.to_owned(),
        paused_scopes: updated,
    }))
}

/// The customer identifier is stored as `customers.client_reference_id`: 1 to 200 characters
/// (design D6).
pub(super) fn validate_external_id(external_id: &str) -> ApiResult<()> {
    if external_id.is_empty() || external_id.chars().count() > 200 {
        return Err(ApiError::invalid_param(
            "account_id",
            "account_id must contain 1 to 200 characters",
        ));
    }
    Ok(())
}

fn validate_reason(reason: &str) -> ApiResult<()> {
    if reason.trim().is_empty() || reason.len() > 1024 {
        return Err(ApiError::bad_request("reason must contain 1 to 1024 bytes"));
    }
    Ok(())
}

/// Validates an account's credentials: an ed25519 public key and a webhook URL.
fn validate_credentials(state: &AppState, public_key: &str, webhook_url: &str) -> ApiResult<()> {
    VerificationKey::from_base64(String::new(), public_key).map_err(|_| {
        ApiError::bad_request("public_key must be standard base64 of a 32-byte ed25519 key")
    })?;
    let local_stack = state.public_origin.to_string().starts_with("http://");
    validate_webhook_url(webhook_url, local_stack)
}

/// Requires an absolute `https` URL without credentials or fragment. `http` is accepted only
/// when the service's own public origin is `http`, which only local stacks use.
fn validate_webhook_url(webhook_url: &str, allow_http: bool) -> ApiResult<()> {
    const MESSAGE: &str = "webhook_url must be an absolute https URL without credentials";
    if webhook_url.len() > 2048 {
        return Err(ApiError::bad_request(MESSAGE));
    }
    let url = url::Url::parse(webhook_url).map_err(|_| ApiError::bad_request(MESSAGE))?;
    let scheme_allowed = url.scheme() == "https" || (allow_http && url.scheme() == "http");
    if !scheme_allowed
        || url.host_str().is_none_or(str::is_empty)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(ApiError::bad_request(MESSAGE));
    }
    Ok(())
}

fn validate_scopes(scopes: Vec<String>) -> ApiResult<Vec<String>> {
    if scopes.is_empty() {
        return Err(ApiError::bad_request("at least one scope is required"));
    }
    let mut validated = scopes
        .into_iter()
        .map(|scope| {
            PauseScope::from_str(&scope)
                .map(|parsed| parsed.code().to_owned())
                .map_err(|_| ApiError::bad_request(format!("unknown pause scope `{scope}`")))
        })
        .collect::<Result<Vec<_>, _>>()?;
    validated.sort();
    validated.dedup();
    Ok(validated)
}

fn decode_nonce(value: &str) -> ApiResult<Vec<u8>> {
    if value.is_empty() || value.len() > 64 {
        return Err(ApiError::bad_request(
            "nonce must be 1 to 32 bytes of hexadecimal",
        ));
    }
    hex::decode(value).map_err(|_| ApiError::bad_request("nonce must be valid hexadecimal"))
}

fn account_response(issued: IssuedAccount) -> AccountResponse {
    AccountResponse {
        key_id: format!("{}/v1", issued.account.public_id),
        id: issued.account.public_id,
        name: issued.account.name,
        livemode: issued.livemode,
        public_key: issued.public_key,
        webhook_url: issued.webhook_url,
        paused_scopes: issued.account.paused_scopes,
    }
}

fn parse_account_id(id: &str) -> ApiResult<Uuid> {
    crate::ids::parse(crate::ids::ACCOUNT, id).ok_or_else(ApiError::not_found)
}

fn parse_refund_id(id: &str) -> ApiResult<Uuid> {
    crate::ids::parse_or_uuid(crate::ids::REFUND, id).ok_or_else(ApiError::not_found)
}

fn admin_actor(state: &AppState) -> Actor {
    Actor::admin(state.admin_key.kid.clone())
}

#[cfg(test)]
mod tests {
    use super::validate_webhook_url;

    #[test]
    fn webhook_urls_use_https_unless_the_service_origin_is_http() {
        assert!(validate_webhook_url("https://product.example/webhooks", false).is_ok());
        assert!(validate_webhook_url("http://product.example/webhooks", false).is_err());
        assert!(validate_webhook_url("http://product:8089/webhooks", true).is_ok());
        for url in [
            "product.example/webhooks",
            "ftp://product.example/webhooks",
            "https://user@product.example/webhooks",
            "https://product.example/webhooks#fragment",
        ] {
            assert!(validate_webhook_url(url, true).is_err(), "{url}");
        }
    }
}
