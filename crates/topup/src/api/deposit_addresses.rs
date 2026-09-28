//! Deposit addresses (`/v1/deposit_addresses`): a customer's persistent, rotatable address for
//! every supported token on every supported chain, credited at spot
//! (docs/design/multi-tenant.md "Deposit addresses").

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use topup_core::route::RouteFile;

use crate::db::{Account, Customer};
use crate::deposit_addresses::{self, ChainContracts, DepositAddressError, ListFilter, Status};
use crate::ids;
use crate::routes::RouteSet;
use crate::tenancy::Permission;

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, ApiPath, query_pairs};
use super::metadata::{self, MetadataUpdate, Object};
use super::models::{
    CreateDepositAddressRequest, DepositAddress, DepositAddressAsset, DepositAddressList,
    DepositAddressNetwork, UpdateMetadataRequest,
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
            description = "`deposit_address_cap_exceeded`, `paused`, `chain_frozen` (a new \
                           address and every chain is frozen or paused), `treasury_not_set` \
                           (no treasury on any chain that takes one), or \
                           `idempotency_key_in_use`",
            body = ErrorResponse
        )
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// Returns the customer's active deposit address, one address for every supported token on every
/// supported network of the key's mode where you have a treasury, issuing it if the customer has
/// none: the same request always returns the same address until it is rotated. It also adds the
/// address's network on a chain supported, or given a treasury, since it was issued, and replaces
/// a chain's network whose treasury changed.
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
    let customer =
        repository::ensure_customer(&state.pool, merchant.scope, &request.client_reference_id)
            .await?;
    let issuable = issuable_chains(&state, &merchant.account, &customer).await?;
    let (address, _) = deposit_addresses::create(
        &state.pool,
        &merchant.account,
        &customer,
        &issuable.chains,
        metadata.as_ref(),
    )
    .await
    .map_err(|error| issuable.map_error(error))?;
    deposit_address_object(&state.routes, &address).map(Json)
}

#[utoipa::path(
    get,
    path = "/v1/deposit_addresses",
    params(
        ("client_reference_id" = Option<String>, Query, description = "Only this customer's addresses"),
        ("status" = Option<String>, Query, description = "`active` or `retired`"),
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
                           `chain_frozen` (every chain is frozen or paused), \
                           `treasury_not_set`, or `idempotency_key_in_use`",
            body = ErrorResponse
        ),
        (status = 429, description = "Too Many Requests: the customer's rotation limit", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "deposit_addresses"
)]
/// Retires an active deposit address and returns the customer's new one, a new address on every
/// network. Payments to the retired address are still credited at spot; stop showing it.
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
    let customer =
        repository::find_customer(&state.pool, merchant.scope, &current.client_reference_id)
            .await?
            .ok_or_else(ApiError::internal)?;
    let issuable = issuable_chains(&state, &merchant.account, &customer).await?;
    let address = deposit_addresses::rotate(
        &state.pool,
        &merchant.account,
        merchant.scope,
        &merchant.actor(),
        id,
        &issuable.chains,
    )
    .await
    .map_err(|error| issuable.map_error(error))?;
    deposit_address_object(&state.routes, &address).map(Json)
}

/// The chains a customer's deposit address gets new networks on, and why a chain was left out.
struct Issuable {
    chains: Vec<ChainContracts>,
    frozen: bool,
}

impl Issuable {
    /// Maps a failure, naming why no chain accepted a new address when none did.
    fn map_error(&self, error: DepositAddressError) -> ApiError {
        match error {
            DepositAddressError::NoChain if self.frozen => ApiError::chain_frozen(),
            DepositAddressError::NoChain => ApiError::paused("new addresses are paused"),
            error => map_error(error),
        }
    }
}

/// New addresses and networks are issued only while `quotes` is not paused for the account or the
/// customer (design §12: no new addresses while paused), and only on the chains of the customer's
/// mode that are not frozen and have a current route not paused for `quotes`. A network pays the
/// account's current treasury of its chain; a chain without one gets no network.
async fn issuable_chains(
    state: &AppState,
    account: &Account,
    customer: &Customer,
) -> ApiResult<Issuable> {
    let paused = |scopes: &[String]| scopes.iter().any(|scope| scope == "quotes");
    if paused(&account.paused_scopes) || paused(&customer.paused_scopes) {
        return Err(ApiError::paused("new addresses are paused"));
    }
    let mut by_chain = BTreeMap::<u64, Vec<&RouteFile>>::new();
    for route in state.routes.current_in(customer.livemode) {
        by_chain
            .entry(route.chain.chain_id)
            .or_default()
            .push(route);
    }
    let mut issuable = Issuable {
        chains: Vec::new(),
        frozen: false,
    };
    for (chain_id, routes) in by_chain {
        if crate::reconciler::chain_is_blocked(&state.pool, chain_id).await? {
            issuable.frozen = true;
            continue;
        }
        let mut open = None;
        for route in routes {
            let scopes = repository::route_paused_scopes(&state.pool, &route.route).await?;
            if !paused(&scopes) {
                open = Some(route);
                break;
            }
        }
        if let Some(route) = open {
            issuable.chains.push(ChainContracts::of(route));
        }
    }
    Ok(issuable)
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

/// The API representation of a deposit address: its networks on the chains that have a current
/// route in its mode, each with those routes' tokens.
pub(crate) fn deposit_address_object(
    routes: &RouteSet,
    address: &deposit_addresses::DepositAddress,
) -> ApiResult<DepositAddress> {
    let mut assets = BTreeMap::<u64, Vec<&RouteFile>>::new();
    for route in routes.current_in(address.livemode) {
        assets.entry(route.chain.chain_id).or_default().push(route);
    }
    let networks: Vec<DepositAddressNetwork> = address
        .networks
        .iter()
        .filter_map(|network| {
            let routes = assets.get(&network.chain_id)?;
            Some(DepositAddressNetwork {
                chain_id: network.chain_id,
                address: format!("{:#x}", network.address),
                treasury: format!("{:#x}", network.treasury),
                assets: routes
                    .iter()
                    .map(|route| DepositAddressAsset {
                        asset: route.asset.symbol.clone(),
                        contract: format!("{:#x}", route.asset.contract),
                        decimals: route.asset.decimals,
                        payment_uri: format!(
                            "ethereum:{:#x}@{}/transfer?address={:#x}",
                            route.asset.contract, network.chain_id, network.address
                        ),
                    })
                    .collect(),
            })
        })
        .collect();
    let shared = networks
        .first()
        .map(|first| &first.address)
        .filter(|first| networks.iter().all(|network| &network.address == *first));
    Ok(DepositAddress {
        id: deposit_addresses::public_id(address.id),
        object: "deposit_address".to_owned(),
        livemode: address.livemode,
        client_reference_id: address.client_reference_id.clone(),
        address: shared.cloned(),
        version: address.version,
        salt: format!("{:#x}", address.salt),
        status: address.status.code().to_owned(),
        created: address.created_at.timestamp(),
        retired_at: address.retired_at.map(|at| at.timestamp()),
        metadata: address.metadata.clone(),
        networks,
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
        DepositAddressError::NoChain => ApiError::paused("new addresses are paused"),
        DepositAddressError::NoTreasury => ApiError::treasury_not_set(),
        DepositAddressError::InvalidInput(message) => ApiError::bad_request(message),
        DepositAddressError::Metadata(error) => error,
        DepositAddressError::DatabaseInvariant => ApiError::internal(),
        DepositAddressError::Database(error) => ApiError::from(error),
    }
}
