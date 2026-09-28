//! Deposit addresses (`/v1/deposit_addresses`): a customer's persistent, rotatable address per
//! chain and asset, credited at spot (docs/design/multi-tenant.md "Deposit addresses").

use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use topup_core::route::RouteFile;

use crate::db::{Account, Customer};
use crate::deposit_addresses::{self, DepositAddressError, ListFilter, Status};
use crate::ids;
use crate::routes::RouteSet;
use crate::tenancy::{Permission, Scope};

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, query_pairs};
use super::metadata::{self, MetadataUpdate, Object};
use super::models::{
    CreateDepositAddressRequest, DepositAddress, DepositAddressList, UpdateMetadataRequest,
};
use super::repository;

type ApiResult<T> = Result<T, ApiError>;

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;

#[utoipa::path(
    post,
    path = "/v1/deposit_addresses",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = CreateDepositAddressRequest,
    responses(
        (status = 200, description = "OK", body = DepositAddress),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (
            status = 409,
            description = "`deposit_address_cap_exceeded`, `paused`, `chain_frozen`, or \
                           `idempotency_key_in_use`",
            body = ErrorResponse
        )
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// Returns the customer's active deposit address for `chain_id` and `asset`, issuing one if it
/// has none: the same request always returns the same address until it is rotated.
pub(crate) async fn create_deposit_address(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiJson(request): ApiJson<CreateDepositAddressRequest>,
) -> ApiResult<Json<DepositAddress>> {
    merchant
        .require(&state.pool, Permission::DepositAddressesWrite)
        .await?;
    validate_client_reference_id(&request.client_reference_id)?;
    let metadata = request
        .metadata
        .as_ref()
        .map(MetadataUpdate::parse)
        .transpose()?;
    let route = payable_route(&state, merchant.scope, request.chain_id, &request.asset)?;
    let customer =
        repository::ensure_customer(&state.pool, merchant.scope, &request.client_reference_id)
            .await?;
    require_issuable(&state, &merchant.account, &customer, route).await?;
    let (address, _) = deposit_addresses::create(
        &state.pool,
        &merchant.account,
        &customer,
        route,
        metadata.as_ref(),
    )
    .await
    .map_err(map_error)?;
    deposit_address_object(&state.routes, &address).map(Json)
}

#[utoipa::path(
    get,
    path = "/v1/deposit_addresses",
    params(
        ("client_reference_id" = Option<String>, Query, description = "Only this customer's addresses"),
        ("status" = Option<String>, Query, description = "`active` or `retired`"),
        ("chain_id" = Option<u64>, Query, description = "Only this chain's addresses"),
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10"),
        ("starting_after" = Option<String>, Query, description = "`da_` id: the page after it"),
        ("ending_before" = Option<String>, Query, description = "`da_` id: the page before it")
    ),
    responses(
        (status = 200, description = "OK", body = DepositAddressList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// The account's deposit addresses in the key's mode, newest first, with Stripe's cursor
/// pagination.
pub(crate) async fn list_deposit_addresses(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<DepositAddressList>> {
    merchant
        .require(&state.pool, Permission::DepositAddressesRead)
        .await?;
    let filter = parse_filter(&query_pairs(query.as_deref()))?;
    let (addresses, has_more) = deposit_addresses::list(&state.pool, merchant.scope, &filter)
        .await
        .map_err(|error| match error {
            DepositAddressError::NotFound => ApiError::invalid_param(
                if filter.before {
                    "ending_before"
                } else {
                    "starting_after"
                },
                "no such deposit address",
            ),
            error => map_error(error),
        })?;
    let data = addresses
        .iter()
        .map(|address| deposit_address_object(&state.routes, address))
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(DepositAddressList {
        object: "list".to_owned(),
        url: "/v1/deposit_addresses".to_owned(),
        has_more,
        data,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/deposit_addresses/{id}",
    params(("id" = String, Path, description = "Deposit address id, `da_…`")),
    responses(
        (status = 200, description = "OK", body = DepositAddress),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// One deposit address.
pub(crate) async fn get_deposit_address(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<DepositAddress>> {
    merchant
        .require(&state.pool, Permission::DepositAddressesRead)
        .await?;
    let id = ids::parse(ids::DEPOSIT_ADDRESS, &id).ok_or_else(ApiError::not_found)?;
    let address = deposit_addresses::get(&state.pool, merchant.scope, id)
        .await
        .map_err(map_error)?
        .ok_or_else(ApiError::not_found)?;
    deposit_address_object(&state.routes, &address).map(Json)
}

#[utoipa::path(
    post,
    path = "/v1/deposit_addresses/{id}",
    params(
        ("id" = String, Path, description = "Deposit address id, `da_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    request_body = UpdateMetadataRequest,
    responses(
        (status = 200, description = "OK", body = DepositAddress),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// Updates a deposit address's `metadata`, active or retired; parameters not sent are left
/// unchanged. Deposits already recorded keep their own copy.
pub(crate) async fn update_deposit_address(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
    ApiJson(request): ApiJson<UpdateMetadataRequest>,
) -> ApiResult<Json<DepositAddress>> {
    merchant
        .require(&state.pool, Permission::DepositAddressesWrite)
        .await?;
    let id = ids::parse(ids::DEPOSIT_ADDRESS, &id).ok_or_else(ApiError::not_found)?;
    if !metadata::update(
        &state.pool,
        Object::DepositAddress,
        merchant.scope,
        id,
        request.metadata.as_ref(),
    )
    .await?
    {
        return Err(ApiError::not_found());
    }
    let address = deposit_addresses::get(&state.pool, merchant.scope, id)
        .await
        .map_err(map_error)?
        .ok_or_else(ApiError::not_found)?;
    deposit_address_object(&state.routes, &address).map(Json)
}

#[utoipa::path(
    post,
    path = "/v1/deposit_addresses/{id}/rotate",
    params(
        ("id" = String, Path, description = "Deposit address id, `da_…`"),
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; for 24 hours a repeat of the same request \
                           returns the first response, and of another request is \
                           `400 idempotency_error`."
        )
    ),
    responses(
        (status = 200, description = "OK: the new active address", body = DepositAddress),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse),
        (
            status = 409,
            description = "`deposit_address_retired`: already rotated; `paused`, \
                           `chain_frozen`, or `idempotency_key_in_use`",
            body = ErrorResponse
        ),
        (status = 429, description = "Too Many Requests: the customer's rotation limit", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// Retires an active deposit address and returns the customer's new one for the same chain and
/// asset. Payments to the retired address are still credited at spot; stop showing it.
pub(crate) async fn rotate_deposit_address(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiPath(id): ApiPath<String>,
) -> ApiResult<Json<DepositAddress>> {
    merchant
        .require(&state.pool, Permission::DepositAddressesWrite)
        .await?;
    let id = ids::parse(ids::DEPOSIT_ADDRESS, &id).ok_or_else(ApiError::not_found)?;
    let current = deposit_addresses::get(&state.pool, merchant.scope, id)
        .await
        .map_err(map_error)?
        .ok_or_else(ApiError::not_found)?;
    let route = payable_route(&state, merchant.scope, current.chain_id, &current.asset)?;
    let customer =
        repository::find_customer(&state.pool, merchant.scope, &current.client_reference_id)
            .await?
            .ok_or_else(ApiError::internal)?;
    require_issuable(&state, &merchant.account, &customer, route).await?;
    let address = deposit_addresses::rotate(
        &state.pool,
        &merchant.account,
        merchant.scope,
        &merchant.actor(),
        id,
        route,
    )
    .await
    .map_err(map_error)?;
    deposit_address_object(&state.routes, &address).map(Json)
}

/// The current route of the scope's mode for `chain_id` and `asset`.
fn payable_route<'a>(
    state: &'a AppState,
    scope: Scope,
    chain_id: u64,
    asset: &str,
) -> ApiResult<&'a RouteFile> {
    state
        .routes
        .current_in(scope.livemode())
        .find(|route| route.chain.chain_id == chain_id && route.asset.symbol == asset)
        .ok_or_else(|| {
            ApiError::invalid_param("asset", "no payable asset matches chain_id and asset")
        })
}

/// New addresses are issued only on an unfrozen chain and while `quotes` is not paused for the
/// account, the customer, or the route (design §12: no new addresses while paused).
async fn require_issuable(
    state: &AppState,
    account: &Account,
    customer: &Customer,
    route: &RouteFile,
) -> ApiResult<()> {
    if crate::reconciler::chain_is_blocked(&state.pool, route.chain.chain_id).await? {
        return Err(ApiError::chain_frozen());
    }
    let route_scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
    let paused = |scopes: &[String]| scopes.iter().any(|scope| scope == "quotes");
    if paused(&account.paused_scopes) || paused(&customer.paused_scopes) || paused(&route_scopes) {
        return Err(ApiError::paused("new addresses are paused"));
    }
    Ok(())
}

fn validate_client_reference_id(client_reference_id: &str) -> ApiResult<()> {
    if client_reference_id.is_empty() || client_reference_id.chars().count() > 200 {
        return Err(ApiError::invalid_param(
            "client_reference_id",
            "client_reference_id must contain 1 to 200 characters",
        ));
    }
    Ok(())
}

/// The API representation of a deposit address.
pub(crate) fn deposit_address_object(
    routes: &RouteSet,
    address: &deposit_addresses::DepositAddress,
) -> ApiResult<DepositAddress> {
    // Any version of the address's route names the same chain and token.
    let route = routes
        .routes()
        .iter()
        .find(|route| route.route == address.route)
        .ok_or_else(|| {
            tracing::error!(route = %address.route, "deposit address route is not loaded");
            ApiError::internal()
        })?;
    Ok(DepositAddress {
        id: deposit_addresses::public_id(address.id),
        object: "deposit_address".to_owned(),
        livemode: address.livemode,
        client_reference_id: address.client_reference_id.clone(),
        chain_id: address.chain_id,
        asset: address.asset.clone(),
        address: format!("{:#x}", address.address),
        payment_uri: format!(
            "ethereum:{:#x}@{}/transfer?address={:#x}",
            route.asset.contract, address.chain_id, address.address
        ),
        treasury: format!("{:#x}", address.treasury),
        version: address.version,
        salt: format!("{:#x}", address.salt),
        status: address.status.code().to_owned(),
        created: address.created_at.timestamp(),
        retired_at: address.retired_at.map(|at| at.timestamp()),
        metadata: address.metadata.clone(),
    })
}

fn parse_filter(pairs: &[(String, String)]) -> ApiResult<ListFilter> {
    let mut filter = ListFilter {
        limit: DEFAULT_LIMIT,
        ..ListFilter::default()
    };
    let mut starting_after = None;
    let mut ending_before = None;
    for (name, value) in pairs {
        match name.as_str() {
            "client_reference_id" => filter.client_reference_id = Some(value.clone()),
            "status" => {
                filter.status = Some(match value.as_str() {
                    "active" => Status::Active,
                    "retired" => Status::Retired,
                    _ => return Err(ApiError::invalid_param("status", "unknown status")),
                });
            }
            "chain_id" => {
                filter.chain_id = Some(value.parse::<u64>().map_err(|_| {
                    ApiError::invalid_param("chain_id", "chain_id must be an integer")
                })?);
            }
            "limit" => {
                filter.limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            "starting_after" | "ending_before" => {
                let id = ids::parse(ids::DEPOSIT_ADDRESS, value)
                    .ok_or_else(|| ApiError::invalid_param(name.clone(), "not a da_ id"))?;
                if name == "starting_after" {
                    starting_after = Some(id);
                } else {
                    ending_before = Some(id);
                }
            }
            other => {
                return Err(
                    ApiError::unknown_param(format!("unknown parameter {other}")).with_param(other),
                );
            }
        }
    }
    match (starting_after, ending_before) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "starting_after and ending_before are mutually exclusive",
            ));
        }
        (Some(id), None) => filter.cursor = Some(id),
        (None, Some(id)) => {
            filter.cursor = Some(id);
            filter.before = true;
        }
        (None, None) => {}
    }
    Ok(filter)
}

fn map_error(error: DepositAddressError) -> ApiError {
    match error {
        DepositAddressError::NotFound => ApiError::not_found(),
        DepositAddressError::Retired => ApiError::deposit_address_retired(),
        error @ DepositAddressError::CapReached(_) => {
            ApiError::deposit_address_cap(error.to_string())
        }
        DepositAddressError::RateLimited => ApiError::rotation_rate_limited(),
        DepositAddressError::InvalidInput(message) => ApiError::bad_request(message),
        DepositAddressError::Metadata(error) => error,
        DepositAddressError::DatabaseInvariant => ApiError::internal(),
        DepositAddressError::Database(error) => ApiError::from(error),
    }
}
