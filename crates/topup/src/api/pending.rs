//! Display-only views of transfers seen before finality (architecture §12).
//!
//! Support, amount matching, and timeliness are computed here at read time and never stored, so
//! an unfinalized transfer can never produce a stored rejection or credit.

use alloy_primitives::Address as EvmAddress;
use axum::Json;
use axum::extract::{Extension, Path, State};
use chrono::{DateTime, TimeDelta, Utc};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use topup_core::valuation::amount_within_tolerance;
use uuid::Uuid;

use crate::db::{self, PendingTransfer, Product};
use crate::locks::RateLock;

use super::AppState;
use super::error::{ApiError, ErrorResponse};
use super::handlers::require_account;
use super::models::{PendingDepositResponse, PendingDepositsResponse, RateLockPayment};

/// Typical Ethereum delay from inclusion to the `finalized` tag: a block in epoch `n` is final
/// once the checkpoint of epoch `n + 1` finalizes, 64 to 95 slots of 12 s (12.8 to 19 minutes).
/// This is an estimate for display; it is not tied to a chain's beacon genesis.
const ESTIMATED_FINALITY_DELAY: TimeDelta = TimeDelta::minutes(15);

#[utoipa::path(
    get,
    path = "/v1/products/{p}/accounts/{ext}/pending-deposits",
    params(
        ("p" = String, Path, description = "Product slug"),
        ("ext" = String, Path, description = "Product-owned account identifier")
    ),
    responses(
        (status = 200, body = PendingDepositsResponse),
        (status = 401, body = ErrorResponse),
        (status = 404, body = ErrorResponse)
    ),
    security(("http_message_signature" = [])),
    tag = "deposits"
)]
/// Transfers to the account's persistent addresses seen above the finalized head. These are not
/// deposits and have not been credited; once final they leave this list and appear under
/// `deposits`, and a reorg can remove them.
pub(crate) async fn list_pending_deposits(
    State(state): State<AppState>,
    Extension(product): Extension<Product>,
    Path((_product_slug, external_id)): Path<(String, String)>,
) -> Result<Json<PendingDepositsResponse>, ApiError> {
    let account = require_account(&state, product.id, &external_id).await?;
    db::touch_persistent_requested(&state.pool, account.id).await?;
    let pending_deposits = db::list_account_pending(&state.pool, account.id)
        .await?
        .into_iter()
        .map(|transfer| PendingDepositResponse {
            supported: product_accepts(
                &state,
                &product,
                transfer.chain_id,
                transfer.asset_contract,
            ),
            deposit_id: transfer.deposit_id,
            chain_id: transfer.chain_id,
            tx_hash: format!("{:#x}", transfer.tx_hash),
            log_index: transfer.log_index,
            block_number: transfer.block_number,
            block_time: transfer.block_time,
            confirmations: transfer.confirmations(),
            address: format!("{:#x}", transfer.address),
            asset_contract: format!("{:#x}", transfer.asset_contract),
            from_address: format!("{:#x}", transfer.from_address),
            amount_atomic: transfer.amount_atomic.value().to_string(),
            first_seen_at: transfer.first_seen_at,
            estimated_final_at: estimated_final_at(transfer.block_time),
        })
        .collect();
    Ok(Json(PendingDepositsResponse { pending_deposits }))
}

/// The first payment to a lock address: its deposit once final, otherwise the earliest transfer
/// seen above the finalized head.
pub(super) async fn lock_payment(
    state: &AppState,
    route: &RouteFile,
    lock: &RateLock,
) -> Result<Option<RateLockPayment>, ApiError> {
    if let Some(deposit) = first_address_deposit(state, lock.address_id).await? {
        return Ok(Some(payment(route, lock, "finalized", deposit, None, None)));
    }
    Ok(db::first_address_pending(&state.pool, lock.address_id)
        .await?
        .map(|transfer| {
            let confirmations = transfer.confirmations();
            let estimated = estimated_final_at(transfer.block_time);
            payment(
                route,
                lock,
                "seen",
                Observed::from(transfer),
                Some(confirmations),
                Some(estimated),
            )
        }))
}

struct Observed {
    chain_id: u64,
    tx_hash: alloy_primitives::B256,
    log_index: u64,
    block_number: u64,
    block_time: DateTime<Utc>,
    asset_contract: EvmAddress,
    amount_atomic: AtomicAmount,
}

impl From<PendingTransfer> for Observed {
    fn from(transfer: PendingTransfer) -> Self {
        Self {
            chain_id: transfer.chain_id,
            tx_hash: transfer.tx_hash,
            log_index: transfer.log_index,
            block_number: transfer.block_number,
            block_time: transfer.block_time,
            asset_contract: transfer.asset_contract,
            amount_atomic: transfer.amount_atomic,
        }
    }
}

fn payment(
    route: &RouteFile,
    lock: &RateLock,
    status: &str,
    observed: Observed,
    confirmations: Option<u64>,
    estimated_final_at: Option<DateTime<Utc>>,
) -> RateLockPayment {
    let supported = observed.chain_id == route.chain.chain_id
        && observed.asset_contract == route.asset.contract;
    RateLockPayment {
        status: status.to_owned(),
        deposit_id: deposit_id(observed.chain_id, observed.tx_hash, observed.log_index),
        tx_hash: format!("{:#x}", observed.tx_hash),
        log_index: observed.log_index,
        block_number: observed.block_number,
        confirmations,
        amount_atomic: observed.amount_atomic.value().to_string(),
        asset_contract: format!("{:#x}", observed.asset_contract),
        supported,
        amount_within_tolerance: supported
            && amount_within_tolerance(
                observed.amount_atomic,
                lock.amount_atomic,
                route.rate_lock.lock_tolerance_bps,
            ),
        in_time: observed.block_time <= lock.expires_at,
        estimated_final_at,
    }
}

async fn first_address_deposit(
    state: &AppState,
    address_id: Uuid,
) -> Result<Option<Observed>, ApiError> {
    let row = sqlx::query_as::<_, (i64, String, i64, i64, DateTime<Utc>, String, String)>(
        r#"
        SELECT chain_id, tx_hash, log_index, block_number, block_time, asset_contract,
               amount_atomic::text
        FROM deposits
        WHERE address_id = $1
        ORDER BY block_number, log_index
        LIMIT 1
        "#,
    )
    .bind(address_id)
    .fetch_optional(&state.pool)
    .await?;
    let Some((chain_id, tx_hash, log_index, block_number, block_time, asset, amount)) = row else {
        return Ok(None);
    };
    let decoded = (|| -> Option<Observed> {
        Some(Observed {
            chain_id: u64::try_from(chain_id).ok()?,
            tx_hash: tx_hash.parse().ok()?,
            log_index: u64::try_from(log_index).ok()?,
            block_number: u64::try_from(block_number).ok()?,
            block_time,
            asset_contract: asset.parse().ok()?,
            amount_atomic: AtomicAmount::new(amount.parse().ok()?),
        })
    })();
    decoded.map(Some).ok_or_else(ApiError::internal)
}

fn product_accepts(
    state: &AppState,
    product: &Product,
    chain_id: u64,
    asset_contract: EvmAddress,
) -> bool {
    state.routes.iter().any(|route| {
        route.destination.product == product.slug
            && route.chain.chain_id == chain_id
            && route.asset.contract == asset_contract
    })
}

fn estimated_final_at(block_time: DateTime<Utc>) -> DateTime<Utc> {
    block_time
        .checked_add_signed(ESTIMATED_FINALITY_DELAY)
        .unwrap_or(block_time)
}
