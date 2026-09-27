//! Deposits (`/v1/deposits`) and refunds (`/v1/refunds`).

use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256, U256};
use axum::Json;
use axum::extract::{Extension, Path, RawQuery, State};
use axum::http::HeaderMap;
use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool, Postgres, QueryBuilder};
use topup_core::money::AtomicAmount;
use uuid::Uuid;

use crate::db::Product;
use crate::ids;
use crate::routes::RouteSet;

use super::AppState;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiJson, expansions, idempotency_key, query_pairs};
use super::models::{
    CreateRefundRequest, Deposit, DepositList, ExpandableDeposit, ExpandableQuote, Refund,
};
use super::repository::{self, NewRefund};

type ApiResult<T> = Result<T, ApiError>;

const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;
const DEPOSIT_STATES: [&str; 5] = ["detected", "confirmed", "credited", "swept", "rejected"];

#[utoipa::path(
    get,
    path = "/v1/deposits",
    params(
        ("account_id" = Option<String>, Query, description = "Only this account's deposits"),
        ("quote" = Option<String>, Query, description = "Only deposits to this quote's address"),
        ("status" = Option<String>, Query, description = "Only deposits in this status"),
        ("tx_hash" = Option<String>, Query, description = "Only deposits in this transaction"),
        ("created[gte]" = Option<i64>, Query, description = "Created at or after, Unix seconds"),
        ("created[lte]" = Option<i64>, Query, description = "Created at or before, Unix seconds"),
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10"),
        ("starting_after" = Option<String>, Query, description = "`dep_` id: the page after it"),
        ("ending_before" = Option<String>, Query, description = "`dep_` id: the page before it"),
        ("expand[]" = Option<Vec<String>>, Query, description = "`data.quote`")
    ),
    responses(
        (status = 200, description = "OK", body = DepositList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
/// The product's deposits, newest first, with Stripe's cursor pagination.
pub(crate) async fn list_deposits(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<DepositList>> {
    let pairs = query_pairs(query.as_deref());
    let expand = expansions(&pairs, &["data.quote"])?;
    let filters = ListFilters::parse(&pairs)?;
    let mut builder = deposit_query();
    builder
        .push(" WHERE account.product_id = ")
        .push_bind(product.id);
    if let Some(account_id) = &filters.account_id {
        builder
            .push(" AND account.external_id = ")
            .push_bind(account_id.clone());
    }
    if let Some(quote) = filters.quote {
        builder
            .push(" AND address.kind = 'lock' AND deposit.address_id = ")
            .push_bind(quote);
    }
    if let Some(status) = &filters.status {
        builder
            .push(" AND deposit.state = ")
            .push_bind(status.clone());
    }
    if let Some(tx_hash) = &filters.tx_hash {
        builder
            .push(" AND deposit.tx_hash = ")
            .push_bind(tx_hash.clone());
    }
    if let Some(from) = filters.created_gte {
        builder.push(" AND deposit.created_at >= ").push_bind(from);
    }
    if let Some(to) = filters.created_lte {
        builder.push(" AND deposit.created_at <= ").push_bind(to);
    }
    let (cursor, before) = match (filters.starting_after, filters.ending_before) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "starting_after and ending_before are mutually exclusive",
            ));
        }
        (Some(id), None) => (Some((id, "starting_after")), false),
        (None, Some(id)) => (Some((id, "ending_before")), true),
        (None, None) => (None, false),
    };
    if let Some((id, param)) = cursor {
        let found: Option<(DateTime<Utc>, Uuid)> = sqlx::query_as(
            r#"
            SELECT deposit.created_at, deposit.id
            FROM deposits AS deposit
            JOIN accounts AS account ON account.id = deposit.account_id
            WHERE deposit.id = $1 AND account.product_id = $2
            "#,
        )
        .bind(id)
        .bind(product.id)
        .fetch_optional(&state.pool)
        .await?;
        let (created_at, id) =
            found.ok_or_else(|| ApiError::invalid_param(param, "no such deposit"))?;
        builder
            .push(if before {
                " AND (deposit.created_at, deposit.id) > ("
            } else {
                " AND (deposit.created_at, deposit.id) < ("
            })
            .push_bind(created_at)
            .push(", ")
            .push_bind(id)
            .push(")");
    }
    builder.push(if before {
        " ORDER BY deposit.created_at ASC, deposit.id ASC LIMIT "
    } else {
        " ORDER BY deposit.created_at DESC, deposit.id DESC LIMIT "
    });
    builder.push_bind(filters.limit.saturating_add(1));
    let mut rows = builder
        .build_query_as::<DepositRow>()
        .fetch_all(&state.pool)
        .await?;
    let has_more = i64::try_from(rows.len()).map_err(|_| ApiError::internal())? > filters.limit;
    rows.truncate(usize::try_from(filters.limit).map_err(|_| ApiError::internal())?);
    if before {
        rows.reverse();
    }
    let mut data = Vec::with_capacity(rows.len());
    for row in rows {
        let mut deposit = deposit_object(&state.routes, row)?;
        if expand.contains(&"data.quote") {
            deposit.quote = expanded_quote(&state, &product, deposit.quote).await?;
        }
        data.push(deposit);
    }
    Ok(Json(DepositList {
        object: "list".to_owned(),
        url: "/v1/deposits".to_owned(),
        has_more,
        data,
    }))
}

#[utoipa::path(
    get,
    path = "/v1/deposits/{id}",
    params(
        ("id" = String, Path, description = "Deposit id, `dep_…`"),
        ("expand[]" = Option<Vec<String>>, Query, description = "`quote`")
    ),
    responses(
        (status = 200, description = "OK", body = Deposit),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
/// One deposit.
pub(crate) async fn get_deposit(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<Deposit>> {
    let expand = expansions(&query_pairs(query.as_deref()), &["quote"])?;
    let id = ids::parse(ids::DEPOSIT, &id).ok_or_else(ApiError::not_found)?;
    let mut deposit = find_deposit(&state.pool, &state.routes, Some(product.id), id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if expand.contains(&"quote") {
        deposit.quote = expanded_quote(&state, &product, deposit.quote).await?;
    }
    Ok(Json(deposit))
}

#[utoipa::path(
    post,
    path = "/v1/refunds",
    params(
        (
            "Idempotency-Key" = Option<String>, Header,
            description = "Up to 255 characters; a repeat with the same parameters returns the \
                           same refund, and with other parameters is `409 idempotency_error`."
        )
    ),
    request_body = CreateRefundRequest,
    responses(
        (status = 200, description = "OK", body = Refund),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (
            status = 409,
            description = "`deposit_not_refundable`, `paused`, `signature_replayed`, or \
                           `idempotency_error`",
            body = ErrorResponse
        )
    ),
    security(("http_message_signature" = [])),
    tag = "refunds"
)]
/// Requests a refund of a deposit for finance's approval (architecture §15): a rejected deposit
/// other than a sanctioned or dust one, or a credited one the product did not apply.
pub(crate) async fn create_refund(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    headers: HeaderMap,
    ApiJson(request): ApiJson<CreateRefundRequest>,
) -> ApiResult<Json<Refund>> {
    let key = idempotency_key(&headers)?;
    let deposit_id = ids::parse(ids::DEPOSIT, &request.deposit)
        .ok_or_else(|| ApiError::invalid_param("deposit", "deposit must be a dep_ id"))?;
    let destination = EvmAddress::from_str(&request.destination_address)
        .ok()
        .filter(|address| !address.is_zero())
        .ok_or_else(|| {
            ApiError::invalid_param(
                "destination_address",
                "destination_address must be a nonzero 20-byte hexadecimal address",
            )
        })?;
    let amount = request
        .amount_atomic
        .as_deref()
        .map(|amount| {
            decimal_u256(amount).map(AtomicAmount::new).ok_or_else(|| {
                ApiError::invalid_param(
                    "amount_atomic",
                    "amount_atomic must be a decimal integer string",
                )
            })
        })
        .transpose()?;
    let route = state.route_for_product(&product)?;
    let refund_id = repository::request_refund(
        &state.pool,
        &NewRefund {
            product_id: product.id,
            deposit_id,
            route,
            destination,
            amount,
            idempotency_key: key.as_deref(),
            actor: &format!("product:{}", product.id),
        },
    )
    .await?;
    let refund = find_refund(&state, product.id, refund_id)
        .await?
        .ok_or_else(ApiError::internal)?;
    Ok(Json(refund))
}

#[utoipa::path(
    get,
    path = "/v1/refunds/{id}",
    params(
        ("id" = String, Path, description = "Refund id, `re_…`"),
        ("expand[]" = Option<Vec<String>>, Query, description = "`deposit`")
    ),
    responses(
        (status = 200, description = "OK", body = Refund),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 404, description = "Not Found", body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "refunds"
)]
/// One refund.
pub(crate) async fn get_refund(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path(id): Path<String>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<Refund>> {
    let expand = expansions(&query_pairs(query.as_deref()), &["deposit"])?;
    let id = ids::parse(ids::REFUND, &id).ok_or_else(ApiError::not_found)?;
    let mut refund = find_refund(&state, product.id, id)
        .await?
        .ok_or_else(ApiError::not_found)?;
    if expand.contains(&"deposit")
        && let ExpandableDeposit::Id(deposit) = &refund.deposit
    {
        let deposit_id = ids::parse(ids::DEPOSIT, deposit).ok_or_else(ApiError::internal)?;
        let deposit = find_deposit(&state.pool, &state.routes, Some(product.id), deposit_id)
            .await?
            .ok_or_else(ApiError::internal)?;
        refund.deposit = ExpandableDeposit::Object(Box::new(deposit));
    }
    Ok(Json(refund))
}

/// Deposit `id`, of `product_id` when given, if it exists.
pub(crate) async fn find_deposit(
    pool: &PgPool,
    routes: &RouteSet,
    product_id: Option<Uuid>,
    id: Uuid,
) -> ApiResult<Option<Deposit>> {
    let mut builder = deposit_query();
    builder.push(" WHERE deposit.id = ").push_bind(id);
    if let Some(product_id) = product_id {
        builder
            .push(" AND account.product_id = ")
            .push_bind(product_id);
    }
    builder
        .build_query_as::<DepositRow>()
        .fetch_optional(pool)
        .await?
        .map(|row| deposit_object(routes, row))
        .transpose()
}

async fn expanded_quote(
    state: &AppState,
    product: &Product,
    quote: Option<ExpandableQuote>,
) -> ApiResult<Option<ExpandableQuote>> {
    let Some(ExpandableQuote::Id(id)) = quote else {
        return Ok(quote);
    };
    let address_id = ids::parse(ids::QUOTE, &id).ok_or_else(ApiError::internal)?;
    let lock = crate::locks::get(&state.pool, product.id, address_id)
        .await
        .map_err(|_| ApiError::internal())?
        .ok_or_else(ApiError::internal)?;
    let quote = super::quotes::quote_object(&state.pool, &state.routes, lock).await?;
    Ok(Some(ExpandableQuote::Object(Box::new(quote))))
}

async fn find_refund(state: &AppState, product_id: Uuid, id: Uuid) -> ApiResult<Option<Refund>> {
    let row = sqlx::query_as::<_, RefundRow>(
        r#"
        SELECT refund.id, refund.deposit_id, refund.amount_atomic::text AS amount_atomic,
               refund.to_address, refund.status, refund.tx_hash, refund.created_at
        FROM refunds AS refund
        JOIN deposits AS deposit ON deposit.id = refund.deposit_id
        JOIN accounts AS account ON account.id = deposit.account_id
        WHERE refund.id = $1 AND account.product_id = $2
        "#,
    )
    .bind(id)
    .bind(product_id)
    .fetch_optional(&state.pool)
    .await?;
    Ok(row.map(|row| Refund {
        id: ids::format(ids::REFUND, row.id),
        object: "refund".to_owned(),
        deposit: ExpandableDeposit::Id(ids::format(ids::DEPOSIT, row.deposit_id)),
        amount_atomic: row.amount_atomic,
        destination_address: row.to_address,
        status: if row.status == "confirmed" {
            "succeeded"
        } else {
            "pending"
        }
        .to_owned(),
        tx_hash: row.tx_hash,
        created: row.created_at.timestamp(),
    }))
}

#[derive(FromRow)]
struct RefundRow {
    id: Uuid,
    deposit_id: Uuid,
    amount_atomic: String,
    to_address: String,
    status: String,
    tx_hash: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(FromRow)]
struct DepositRow {
    id: Uuid,
    external_id: String,
    address_kind: String,
    address_id: Uuid,
    state: String,
    reason: Option<String>,
    chain_id: i64,
    route: Option<String>,
    asset_contract: String,
    amount_atomic: String,
    credit_minor: Option<String>,
    price_scaled: Option<String>,
    price_source: Option<String>,
    valuation_at: Option<DateTime<Utc>>,
    address: String,
    from_address: String,
    tx_hash: String,
    log_index: i64,
    block_number: i64,
    amount_refunded_atomic: String,
    created_at: DateTime<Utc>,
}

fn deposit_query() -> QueryBuilder<Postgres> {
    QueryBuilder::new(
        r#"
        SELECT deposit.id, account.external_id, address.kind AS address_kind, deposit.address_id,
               deposit.state, deposit.reason, deposit.chain_id, deposit.route,
               deposit.asset_contract, deposit.amount_atomic::text AS amount_atomic,
               deposit.credit_minor::text AS credit_minor,
               deposit.price_scaled::text AS price_scaled, deposit.price_source,
               deposit.valuation_at, address.address, deposit.from_address, deposit.tx_hash,
               deposit.log_index, deposit.block_number,
               COALESCE((
                   SELECT sum(refund.amount_atomic)
                   FROM refunds AS refund
                   WHERE refund.deposit_id = deposit.id AND refund.status = 'confirmed'
               ), 0)::text AS amount_refunded_atomic,
               deposit.created_at
        FROM deposits AS deposit
        JOIN accounts AS account ON account.id = deposit.account_id
        JOIN addresses AS address ON address.id = deposit.address_id
        "#,
    )
}

fn deposit_object(routes: &RouteSet, row: DepositRow) -> ApiResult<Deposit> {
    let asset = row.route.as_deref().and_then(|name| {
        routes
            .routes()
            .iter()
            .find(|route| route.route == name)
            .map(|route| route.asset.symbol.clone())
    });
    let refunded = row.amount_refunded_atomic == row.amount_atomic;
    Ok(Deposit {
        id: ids::format(ids::DEPOSIT, row.id),
        object: "deposit".to_owned(),
        account_id: row.external_id,
        quote: (row.address_kind == "lock")
            .then(|| ExpandableQuote::Id(ids::format(ids::QUOTE, row.address_id))),
        status: row.state,
        rejection_reason: row.reason,
        chain_id: u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?,
        asset,
        asset_contract: row.asset_contract,
        amount_atomic: row.amount_atomic,
        amount: row
            .credit_minor
            .map(|credit| credit.parse::<u64>())
            .transpose()
            .map_err(|_| ApiError::internal())?,
        currency: "usd".to_owned(),
        exchange_rate: row
            .price_scaled
            .map(|price| price.parse::<u64>().map(super::quotes::decimal))
            .transpose()
            .map_err(|_| ApiError::internal())?,
        price_source: row.price_source.map(|source| {
            if source == "lock" {
                "quote".to_owned()
            } else {
                source
            }
        }),
        valued_at: row.valuation_at.map(|at| at.timestamp()),
        address: row.address,
        from_address: row.from_address,
        tx_hash: row.tx_hash,
        log_index: u64::try_from(row.log_index).map_err(|_| ApiError::internal())?,
        block_number: u64::try_from(row.block_number).map_err(|_| ApiError::internal())?,
        amount_refunded_atomic: row.amount_refunded_atomic,
        refunded,
        created: row.created_at.timestamp(),
    })
}

struct ListFilters {
    account_id: Option<String>,
    quote: Option<Uuid>,
    status: Option<String>,
    tx_hash: Option<String>,
    created_gte: Option<DateTime<Utc>>,
    created_lte: Option<DateTime<Utc>>,
    limit: i64,
    starting_after: Option<Uuid>,
    ending_before: Option<Uuid>,
}

impl ListFilters {
    fn parse(pairs: &[(String, String)]) -> ApiResult<Self> {
        let mut filters = Self {
            account_id: None,
            quote: None,
            status: None,
            tx_hash: None,
            created_gte: None,
            created_lte: None,
            limit: DEFAULT_LIMIT,
            starting_after: None,
            ending_before: None,
        };
        for (name, value) in pairs {
            match name.as_str() {
                "account_id" => filters.account_id = Some(value.clone()),
                "quote" => {
                    filters.quote = Some(
                        ids::parse(ids::QUOTE, value)
                            .ok_or_else(|| ApiError::invalid_param("quote", "not a qt_ id"))?,
                    );
                }
                "status" => {
                    if !DEPOSIT_STATES.contains(&value.as_str()) {
                        return Err(ApiError::invalid_param("status", "unknown status"));
                    }
                    filters.status = Some(value.clone());
                }
                "tx_hash" => {
                    let hash = B256::from_str(value).map_err(|_| {
                        ApiError::invalid_param("tx_hash", "tx_hash must be 32 bytes of hex")
                    })?;
                    filters.tx_hash = Some(format!("{hash:#x}"));
                }
                "created[gte]" => filters.created_gte = Some(timestamp(name, value)?),
                "created[lte]" => filters.created_lte = Some(timestamp(name, value)?),
                "limit" => {
                    filters.limit = value
                        .parse::<i64>()
                        .ok()
                        .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                        .ok_or_else(|| {
                            ApiError::invalid_param("limit", "limit must be 1 to 100")
                        })?;
                }
                "starting_after" | "ending_before" => {
                    let id = ids::parse(ids::DEPOSIT, value)
                        .ok_or_else(|| ApiError::invalid_param(name.clone(), "not a dep_ id"))?;
                    if name == "starting_after" {
                        filters.starting_after = Some(id);
                    } else {
                        filters.ending_before = Some(id);
                    }
                }
                "expand[]" | "expand" => {}
                other => {
                    return Err(
                        ApiError::unknown_param(format!("unknown parameter {other}"))
                            .with_param(other),
                    );
                }
            }
        }
        Ok(filters)
    }
}

fn timestamp(name: &str, value: &str) -> ApiResult<DateTime<Utc>> {
    value
        .parse::<i64>()
        .ok()
        .and_then(|seconds| DateTime::from_timestamp(seconds, 0))
        .ok_or_else(|| ApiError::invalid_param(name, format!("{name} must be Unix seconds")))
}

fn decimal_u256(value: &str) -> Option<U256> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    U256::from_str(value).ok()
}
