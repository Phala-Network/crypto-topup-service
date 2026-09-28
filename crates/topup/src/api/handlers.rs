//! Typed API handlers for the routes in architecture §12.

use std::str::FromStr;

use axum::Json;
use axum::extract::State;
use axum::http::header;
use axum::response::Response;
use topup_core::screening::PauseScope;
use uuid::Uuid;

use crate::audit::Actor;
use crate::db::Customer;
use crate::tenancy::Scope;

use super::AppState;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath};
use super::models::{
    AccountPauseRequest, AccountResponse, AdminReasonRequest, ApiKeyObject, Contact,
    CreateAccountRequest, CustomerPauseRequest, DailyReportResponse, IssueApiKeyRequest,
    NudgeResponse, OutboxReplayResponse, PauseRequest, PauseResponse,
    ReconciliationBlockLiftResponse, RoutePauseResponse, SupportDepositResponse,
    UpdateAccountRequest,
};
use super::repository::{self, IssuedAccount};

type ApiResult<T> = Result<T, ApiError>;

#[utoipa::path(
    post,
    path = "/v1/admin/accounts",
    request_body = CreateAccountRequest,
    responses(
        (status = 200, description = "OK: created, with its first secret keys", body = AccountResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Creates a merchant account after the operator's offline due diligence (design D8): records the
/// contact and the due diligence, decides live mode (`charges_enabled`, D12), and returns the
/// first secret key of test mode and, with live mode, of live mode. Each key's `secret` is shown
/// only in this response; send it to the contact, who rolls it on receipt. Audited.
pub(crate) async fn create_account(
    State(state): State<AppState>,
    ApiJson(request): ApiJson<CreateAccountRequest>,
) -> ApiResult<Json<AccountResponse>> {
    let name = request.name.trim();
    validate_label("name", name)?;
    validate_contact(&request.contact)?;
    validate_label("due_diligence.reference", &request.due_diligence.reference)?;
    validate_label(
        "due_diligence.reviewed_by",
        &request.due_diligence.reviewed_by,
    )?;
    validate_reason(&request.reason)?;
    if let Some(url) = &request.webhook_url {
        validate_webhook_url(url, local_stack(&state))?;
    }
    let account = repository::create_account(
        &state.pool,
        &repository::NewAccount {
            name,
            contact: to_json(&request.contact)?,
            due_diligence: to_json(&request.due_diligence)?,
            charges_enabled: request.charges_enabled,
            webhook_url: request.webhook_url.as_deref(),
        },
        &admin_actor(&state),
        &request.reason,
    )
    .await?;
    Ok(Json(account_response(account)?))
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}",
    params(("account" = String, Path, description = "Account id, `acct_…`")),
    request_body = UpdateAccountRequest,
    responses(
        (status = 200, description = "OK: updated, or already holding these values", body = AccountResponse),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found: no account has this id", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Updates an account: live mode (enabling it returns the first live key), the restricted flag,
/// the contact, or the webhook URL. Audited, and announced to the account as `account.updated`.
pub(crate) async fn update_account(
    State(state): State<AppState>,
    ApiPath(account): ApiPath<String>,
    ApiJson(request): ApiJson<UpdateAccountRequest>,
) -> ApiResult<Json<AccountResponse>> {
    let account_id = parse_account_id(&account)?;
    if let Some(contact) = &request.contact {
        validate_contact(contact)?;
    }
    if let Some(url) = &request.webhook_url {
        validate_webhook_url(url, local_stack(&state))?;
    }
    validate_reason(&request.reason)?;
    let account = repository::update_account(
        &state.pool,
        account_id,
        &repository::AccountChanges {
            charges_enabled: request.charges_enabled,
            restricted: request.restricted,
            contact: request.contact.as_ref().map(to_json).transpose()?,
            webhook_url: request.webhook_url.as_deref(),
        },
        &admin_actor(&state),
        &request.reason,
    )
    .await?;
    Ok(Json(account_response(account)?))
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}/api_keys",
    params(("account" = String, Path, description = "Account id, `acct_…`")),
    request_body = IssueApiKeyRequest,
    responses(
        (status = 200, description = "OK: the key with its `secret`, shown once", body = ApiKeyObject),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 403, description = "`testmode_charges_only`: a live key for an account without live mode", body = ErrorResponse),
        (status = 404, description = "Not Found: no account has this id", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Issues a recovery key (design D7) after the operator verified the request with the recorded
/// contact, optionally revoking every key of the mode first. Audited, and announced as
/// `api_key.*` events with actor `admin`.
pub(crate) async fn issue_api_key(
    State(state): State<AppState>,
    ApiPath(account): ApiPath<String>,
    ApiJson(request): ApiJson<IssueApiKeyRequest>,
) -> ApiResult<Response> {
    let account_id = parse_account_id(&account)?;
    validate_reason(&request.reason)?;
    super::keys::validate_name(&request.name)?;
    let issued = crate::api_keys::recover(
        &state.pool,
        Scope::new(account_id, request.livemode),
        &request.name,
        request.revoke_existing,
        &admin_actor(&state),
        &request.reason,
    )
    .await
    .map_err(super::keys::map_error)?;
    Ok(super::keys::issued_response(&issued))
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
    path = "/v1/admin/accounts/{account}/pause",
    params(("account" = String, Path, description = "Account id, `acct_…`")),
    request_body = AccountPauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Pauses scopes of a whole account in both modes, for example `quotes` and `settlement` for an
/// abusive account or a sanctioned treasury. Audited, and announced as `account.updated`.
pub(crate) async fn pause_account(
    State(state): State<AppState>,
    ApiPath(account): ApiPath<String>,
    ApiJson(request): ApiJson<AccountPauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &account, request, true).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}/resume",
    params(("account" = String, Path, description = "Account id, `acct_…`")),
    request_body = AccountPauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Resumes scopes of a whole account, for example after a sanctioned treasury was replaced and
/// reviewed. Audited, and announced as `account.updated`.
pub(crate) async fn resume_account(
    State(state): State<AppState>,
    ApiPath(account): ApiPath<String>,
    ApiJson(request): ApiJson<AccountPauseRequest>,
) -> ApiResult<Json<PauseResponse>> {
    mutate_account_scopes(&state, &account, request, false).await
}

#[utoipa::path(
    post,
    path = "/v1/admin/accounts/{account}/customers/{customer}/pause",
    params(
        ("account" = String, Path, description = "Account id, `acct_…`"),
        ("customer" = String, Path, description = "The account's identifier of its customer, the quotes' `account_id`")
    ),
    request_body = CustomerPauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Pauses scopes of one customer of an account, for example `settlement` to stop crediting it.
pub(crate) async fn pause_customer(
    State(state): State<AppState>,
    ApiPath((account, customer)): ApiPath<(String, String)>,
    ApiJson(request): ApiJson<CustomerPauseRequest>,
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
    request_body = CustomerPauseRequest,
    responses((status = 200, description = "OK", body = PauseResponse), (status = 400, description = "Bad Request", body = ErrorResponse), (status = 404, description = "Not Found", body = ErrorResponse)),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
/// Resumes scopes of one customer of an account.
pub(crate) async fn resume_customer(
    State(state): State<AppState>,
    ApiPath((account, customer)): ApiPath<(String, String)>,
    ApiJson(request): ApiJson<CustomerPauseRequest>,
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
    path = "/v1/admin/metrics",
    responses((
        status = 200,
        description = "OK: the process's counters in the Prometheus text format, such as \
                       `topup_rpc_calls_total{provider, chain_id, method}`; they restart at zero \
                       with the process",
        body = String,
        content_type = "text/plain"
    )),
    security(("http_message_signature" = [])),
    tag = "admin"
)]
pub(crate) async fn metrics() -> ([(header::HeaderName, &'static str); 1], String) {
    (
        [(
            header::CONTENT_TYPE,
            crate::observability::metrics::CONTENT_TYPE,
        )],
        crate::observability::metrics::render(),
    )
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
    report.reconciliation = crate::observability::reconciliation().map(Into::into);
    Ok(Json(report))
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

async fn mutate_account_scopes(
    state: &AppState,
    account: &str,
    request: AccountPauseRequest,
    pause: bool,
) -> ApiResult<Json<PauseResponse>> {
    let scopes = validate_scopes(request.scopes)?;
    if request.reason.trim().is_empty() {
        return Err(ApiError::invalid_param("reason", "reason is required"));
    }
    let account_id = parse_account_id(account)?;
    let scopes: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let mut transaction = state.pool.begin().await?;
    let updated = crate::pause::mutate_account_scopes_in(
        &mut transaction,
        account_id,
        &scopes,
        pause,
        &admin_actor(state),
        &request.reason,
    )
    .await?
    .ok_or_else(ApiError::not_found)?;
    transaction.commit().await?;
    Ok(Json(PauseResponse {
        paused_scopes: updated,
    }))
}

async fn mutate_customer_scopes(
    state: &AppState,
    account: &str,
    client_reference_id: &str,
    request: CustomerPauseRequest,
    pause: bool,
) -> ApiResult<Json<PauseResponse>> {
    let scopes = validate_scopes(request.scopes)?;
    let account_id = parse_account_id(account)?;
    let customer = repository::find_customer(
        &state.pool,
        Scope::new(account_id, request.livemode),
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

/// Whether the service's own public origin is `http`, which only local stacks use.
fn local_stack(state: &AppState) -> bool {
    state.public_origin.to_string().starts_with("http://")
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

fn account_response(issued: IssuedAccount) -> ApiResult<AccountResponse> {
    let account = issued.account;
    Ok(AccountResponse {
        id: account.public_id,
        object: "account".to_owned(),
        name: account.name,
        contact: from_json(account.contact)?,
        due_diligence: from_json(account.due_diligence)?,
        charges_enabled: account.charges_enabled,
        restricted: account.restricted,
        paused_scopes: account.paused_scopes,
        created: account.created_at.timestamp(),
        api_keys: issued
            .api_keys
            .iter()
            .map(|key| super::keys::api_key_object(&key.key, Some(key.secret.as_str().to_owned())))
            .collect(),
    })
}

fn to_json<T: serde::Serialize>(value: &T) -> ApiResult<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| {
        tracing::error!(%error, "admin record serialization failed");
        ApiError::internal()
    })
}

fn from_json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> ApiResult<T> {
    serde_json::from_value(value).map_err(|error| {
        tracing::error!(%error, "stored admin record is malformed");
        ApiError::internal()
    })
}

/// A label of 1 to 200 characters.
fn validate_label(param: &str, value: &str) -> ApiResult<()> {
    if value.trim().is_empty() || value.chars().count() > 200 {
        return Err(ApiError::invalid_param(
            param,
            format!("{param} must contain 1 to 200 characters"),
        ));
    }
    Ok(())
}

/// A contact's name and a plausible email address; the operator verifies it offline.
fn validate_contact(contact: &Contact) -> ApiResult<()> {
    validate_label("contact.name", &contact.name)?;
    let email = contact.email.as_str();
    let plausible = email.len() <= 320
        && email
            .split_once('@')
            .is_some_and(|(local, domain)| !local.is_empty() && domain.contains('.'))
        && !email.chars().any(char::is_whitespace);
    if !plausible {
        return Err(ApiError::invalid_param(
            "contact.email",
            "contact.email must be an email address",
        ));
    }
    Ok(())
}

fn parse_account_id(id: &str) -> ApiResult<Uuid> {
    crate::ids::parse(crate::ids::ACCOUNT, id).ok_or_else(ApiError::not_found)
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
