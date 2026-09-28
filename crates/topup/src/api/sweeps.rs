//! The sweep builder (`GET /v1/sweeps`, design D4) and the address export (`GET /v1/addresses`,
//! design §13): the merchant's own reads of what its forwarders hold and how to recompute and
//! sweep them without Phala Pay.

use std::collections::BTreeMap;
use std::str::FromStr;

use alloy_primitives::{Address as EvmAddress, B256};
use axum::Json;
use axum::extract::{Extension, RawQuery, State};
use chrono::{DateTime, Utc};
use sqlx::FromRow;
use topup_adapters::chain::flush::encode_flush;
use uuid::Uuid;

use crate::ids;
use crate::refunds::DestinationScreening;
use crate::tenancy::Permission;

use super::AppState;
use super::auth::Merchant;
use super::error::{ApiError, ErrorResponse};
use super::extract::{ApiQuery, query_pairs};
use super::models::{
    AddressList, AddressObject, Sweep, SweepAddress, SweepList, SweepQuery, SweepTransaction,
};

type ApiResult<T> = Result<T, ApiError>;

/// Forwarders per `flush` call: a bounded calldata size and gas cost per transaction.
const MAX_SWEEP_ADDRESSES: usize = 200;
const DEFAULT_LIMIT: i64 = 10;
const MAX_LIMIT: i64 = 100;

#[utoipa::path(
    get,
    path = "/v1/sweeps",
    params(SweepQuery),
    responses(
        (status = 200, description = "OK", body = SweepList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse),
        (status = 503, description = "Treasury screening is unavailable; retry", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "sweeps"
)]
/// The sweepable balances of the key's mode, by chain, token, and treasury, each with the
/// `factory.flush(treasury, salts, token)` call that moves them (design D4). A forwarder's
/// balance is its final deposits, reversed ones excluded, minus the finalized `Flushed` amounts,
/// so a sweep never counts a deposit that could still be reversed. A forwarder holding a deposit
/// rejected as sanctioned is never listed, and no call is built to a treasury a sanctions list
/// names. Anyone may send the call; the sender pays the gas. The Python SDK's `safe_batch` turns
/// the calls into a Safe Transaction Builder file.
pub(crate) async fn list_sweeps(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    ApiQuery(query): ApiQuery<SweepQuery>,
) -> ApiResult<Json<SweepList>> {
    merchant
        .require(&state.pool, Permission::SweepsRead)
        .await?;
    let scope = merchant.scope;
    let token = query
        .token
        .as_deref()
        .map(|token| {
            EvmAddress::from_str(token)
                .map_err(|_| ApiError::invalid_param("token", "token must be a 20-byte address"))
        })
        .transpose()?;
    let chain_id = query
        .chain_id
        .map(i64::try_from)
        .transpose()
        .map_err(|_| ApiError::invalid_param("chain_id", "chain_id is out of range"))?;
    let rows = sqlx::query_as::<_, SweepRow>(
        r#"
        WITH final_deposits AS (
            SELECT deposit.address_id, deposit.asset_contract AS token,
                   SUM(deposit.amount_atomic) AS total
            FROM deposits AS deposit
            WHERE deposit.account_id = $1 AND deposit.livemode = $2
              AND deposit.final_at IS NOT NULL AND deposit.state <> 'reversed'
            GROUP BY deposit.address_id, deposit.asset_contract
        ), swept AS (
            SELECT flushed.address_id, flushed.token, SUM(flushed.amount_atomic) AS total
            FROM flushed
            JOIN addresses AS address ON address.id = flushed.address_id
            WHERE address.account_id = $1 AND address.livemode = $2
            GROUP BY flushed.address_id, flushed.token
        )
        SELECT address.chain_id, address.address, address.salt, address.treasury,
               held.token, (held.total - COALESCE(swept.total, 0))::text AS amount_atomic
        FROM final_deposits AS held
        JOIN addresses AS address ON address.id = held.address_id
        LEFT JOIN swept ON swept.address_id = held.address_id AND swept.token = held.token
        WHERE held.total > COALESCE(swept.total, 0)
          AND ($3::bigint IS NULL OR address.chain_id = $3)
          AND ($4::text IS NULL OR held.token = $4)
          AND NOT EXISTS (
              SELECT 1 FROM deposits AS sanctioned
              WHERE sanctioned.address_id = held.address_id
                AND sanctioned.asset_contract = held.token
                AND sanctioned.state = 'rejected' AND sanctioned.reason = 'sanctioned'
          )
        ORDER BY address.chain_id, held.token, address.treasury, address.address
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id)
    .bind(token.map(|token| format!("{token:#x}")))
    .fetch_all(&state.pool)
    .await?;

    let mut groups = BTreeMap::<(u64, EvmAddress, EvmAddress), Vec<SweepEntry>>::new();
    for row in rows {
        let chain_id = u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?;
        let salt = B256::from_str(&row.salt).map_err(|_| ApiError::internal())?;
        groups
            .entry((chain_id, parse(&row.token)?, parse(&row.treasury)?))
            .or_default()
            .push((parse(&row.address)?, salt, row.amount_atomic));
    }

    let mut data = Vec::new();
    for ((chain_id, token, treasury), forwarders) in groups {
        let route = state
            .routes
            .current_in(scope.livemode())
            .find(|route| route.chain.chain_id == chain_id);
        // A chain without a current route of the mode has no factory to name and no oracle.
        let Some(route) = route else {
            continue;
        };
        match state.screening.screen(route, treasury).await {
            DestinationScreening::Clear => {}
            DestinationScreening::Sanctioned => continue,
            DestinationScreening::Unavailable => {
                return Err(ApiError::service_unavailable(
                    "sanctions screening of the treasury is unavailable; retry",
                ));
            }
        }
        let asset = state
            .routes
            .current_in(scope.livemode())
            .find(|route| route.chain.chain_id == chain_id && route.asset.contract == token)
            .map(|route| route.asset.symbol.clone());
        let factory = route.chain.contracts.forwarder_factory;
        for chunk in forwarders.chunks(MAX_SWEEP_ADDRESSES) {
            let total = chunk
                .iter()
                .try_fold(alloy_primitives::U256::ZERO, |sum, (_, _, amount)| {
                    alloy_primitives::U256::from_str(amount)
                        .ok()
                        .and_then(|amount| sum.checked_add(amount))
                })
                .ok_or_else(ApiError::internal)?;
            let salts = chunk.iter().map(|(_, salt, _)| *salt).collect();
            data.push(Sweep {
                object: "sweep".to_owned(),
                livemode: scope.livemode(),
                chain_id,
                token: format!("{token:#x}"),
                asset: asset.clone(),
                treasury: format!("{treasury:#x}"),
                factory: format!("{factory:#x}"),
                amount_atomic: total.to_string(),
                addresses: chunk
                    .iter()
                    .map(|(address, salt, amount)| SweepAddress {
                        address: format!("{address:#x}"),
                        salt: format!("{salt:#x}"),
                        amount_atomic: amount.clone(),
                    })
                    .collect(),
                transaction: SweepTransaction {
                    to: format!("{factory:#x}"),
                    data: format!("{:#x}", encode_flush(treasury, salts, token)),
                    value: "0".to_owned(),
                },
            });
        }
    }
    Ok(Json(SweepList {
        object: "list".to_owned(),
        url: "/v1/sweeps".to_owned(),
        has_more: false,
        data,
    }))
}

/// A forwarder, its salt, and its sweepable amount as a decimal string.
type SweepEntry = (EvmAddress, B256, String);

#[derive(FromRow)]
struct SweepRow {
    chain_id: i64,
    address: String,
    salt: String,
    treasury: String,
    token: String,
    amount_atomic: String,
}

fn parse(address: &str) -> ApiResult<EvmAddress> {
    EvmAddress::from_str(address).map_err(|_| ApiError::internal())
}

#[utoipa::path(
    get,
    path = "/v1/addresses",
    params(
        ("chain_id" = Option<u64>, Query, description = "Only this chain's addresses"),
        ("limit" = Option<i64>, Query, description = "1 to 100, default 10"),
        ("starting_after" = Option<String>, Query, description = "`addr_` id: the page after it"),
        ("ending_before" = Option<String>, Query, description = "`addr_` id: the page before it")
    ),
    responses(
        (status = 200, description = "OK", body = AddressList),
        (status = 400, description = "Bad Request", body = ErrorResponse),
        (status = 401, description = "Unauthorized", body = ErrorResponse)
    ),
    security(("api_key" = [])),
    tag = "addresses"
)]
/// Every forwarder address issued in the key's mode, for quotes and for deposit address networks
/// (current and superseded), with the `(factory, implementation, salt, treasury)` it is derived
/// from: the export that keeps funds recomputable and sweepable without Phala Pay (design §13).
/// Pages follow `id` order.
pub(crate) async fn list_addresses(
    State(state): State<AppState>,
    Extension(merchant): Extension<Merchant>,
    RawQuery(query): RawQuery,
) -> ApiResult<Json<AddressList>> {
    merchant
        .require(&state.pool, Permission::AddressesRead)
        .await?;
    let scope = merchant.scope;
    let mut chain_id = None;
    let mut limit = DEFAULT_LIMIT;
    let mut starting_after = None;
    let mut ending_before = None;
    for (name, value) in query_pairs(query.as_deref()) {
        match name.as_str() {
            "chain_id" => {
                chain_id = Some(value.parse::<i64>().ok().filter(|id| *id >= 0).ok_or_else(
                    || ApiError::invalid_param("chain_id", "chain_id must be a chain id"),
                )?);
            }
            "limit" => {
                limit = value
                    .parse::<i64>()
                    .ok()
                    .filter(|limit| (1..=MAX_LIMIT).contains(limit))
                    .ok_or_else(|| ApiError::invalid_param("limit", "limit must be 1 to 100"))?;
            }
            "starting_after" | "ending_before" => {
                let id = ids::parse(ids::ADDRESS, &value)
                    .ok_or_else(|| ApiError::invalid_param(name.clone(), "not an addr_ id"))?;
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
    let (cursor, before) = match (starting_after, ending_before) {
        (Some(_), Some(_)) => {
            return Err(ApiError::bad_request(
                "starting_after and ending_before are mutually exclusive",
            ));
        }
        (Some(id), None) => (Some(id), false),
        (None, Some(id)) => (Some(id), true),
        (None, None) => (None, false),
    };
    let mut rows = sqlx::query_as::<_, AddressRow>(
        r#"
        SELECT address.id, address.livemode, address.chain_id, address.address, address.salt,
               address.treasury, address.quote_id, address.deposit_address_id,
               customer.client_reference_id,
               COALESCE(quote.created_at, deposit_address.created_at) AS created_at
        FROM addresses AS address
        LEFT JOIN quotes AS quote ON quote.id = address.quote_id
        LEFT JOIN deposit_addresses AS deposit_address
            ON deposit_address.id = address.deposit_address_id
        JOIN customers AS customer
            ON customer.id = COALESCE(quote.customer_id, deposit_address.customer_id)
        WHERE address.account_id = $1 AND address.livemode = $2
          AND ($3::bigint IS NULL OR address.chain_id = $3)
          AND ($4::uuid IS NULL OR (CASE WHEN $5 THEN address.id < $4 ELSE address.id > $4 END))
        ORDER BY CASE WHEN $5 THEN address.id END DESC, address.id ASC
        LIMIT $6
        "#,
    )
    .bind(scope.account_id())
    .bind(scope.livemode())
    .bind(chain_id)
    .bind(cursor)
    .bind(before)
    .bind(limit.saturating_add(1))
    .fetch_all(&state.pool)
    .await?;
    let has_more = i64::try_from(rows.len()).map_err(|_| ApiError::internal())? > limit;
    rows.truncate(usize::try_from(limit).map_err(|_| ApiError::internal())?);
    if before {
        rows.reverse();
    }
    let data = rows
        .into_iter()
        .map(|row| {
            let chain_id = u64::try_from(row.chain_id).map_err(|_| ApiError::internal())?;
            let contracts = state.routes.chain(chain_id).ok_or_else(|| {
                tracing::error!(chain_id, "an issued address's chain is not loaded");
                ApiError::internal()
            })?;
            Ok(AddressObject {
                id: ids::format(ids::ADDRESS, row.id),
                object: "address".to_owned(),
                livemode: row.livemode,
                chain_id,
                address: row.address,
                factory: format!("{:#x}", contracts.contracts.forwarder_factory),
                implementation: format!("{:#x}", contracts.contracts.implementation),
                salt: row.salt,
                treasury: row.treasury,
                quote: row.quote_id.map(|id| ids::format(ids::QUOTE, id)),
                deposit_address: row
                    .deposit_address_id
                    .map(crate::deposit_addresses::public_id),
                client_reference_id: row.client_reference_id,
                created: row.created_at.timestamp(),
            })
        })
        .collect::<ApiResult<Vec<_>>>()?;
    Ok(Json(AddressList {
        object: "list".to_owned(),
        url: "/v1/addresses".to_owned(),
        has_more,
        data,
    }))
}

#[derive(FromRow)]
struct AddressRow {
    id: Uuid,
    livemode: bool,
    chain_id: i64,
    address: String,
    salt: String,
    treasury: String,
    quote_id: Option<Uuid>,
    deposit_address_id: Option<Uuid>,
    client_reference_id: String,
    created_at: DateTime<Utc>,
}
