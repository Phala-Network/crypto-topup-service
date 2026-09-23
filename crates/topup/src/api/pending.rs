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
use crate::locks::{RateLock, RateLockStatus};

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
    touch_requested(&state, account.id).await;
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

/// The payment the lock page shows, following the consumption rule of §9: the deposit that
/// consumed the lock; otherwise the first transfer that would consume it (finalized deposits
/// first, then transfers seen above `finalized`); otherwise the first transfer at all. On a
/// cancelled lock no payment is in time or within tolerance, because every payment is valued at
/// spot.
pub(super) async fn lock_payment(
    state: &AppState,
    route: &RouteFile,
    lock: &RateLock,
) -> Result<Option<RateLockPayment>, ApiError> {
    let consumed_by = sqlx::query_scalar::<_, Option<Uuid>>(
        "SELECT consumed_by FROM rate_locks WHERE address_id = $1",
    )
    .bind(lock.address_id)
    .fetch_optional(&state.pool)
    .await?
    .flatten();
    let deposits = address_deposits(state, lock.address_id).await?;
    if let Some(consumed) = deposits
        .iter()
        .find(|deposit| Some(deposit.deposit_id) == consumed_by)
    {
        return Ok(Some(payment(route, lock, consumed)));
    }
    let observed = deposits
        .into_iter()
        .chain(
            db::list_address_pending(&state.pool, lock.address_id)
                .await?
                .into_iter()
                .map(Observed::from),
        )
        .collect::<Vec<_>>();
    let shown = observed
        .iter()
        .find(|candidate| terms(route, lock, candidate).consumes_lock())
        .or_else(|| observed.first());
    Ok(shown.map(|observed| payment(route, lock, observed)))
}

struct Observed {
    status: &'static str,
    deposit_id: Uuid,
    tx_hash: alloy_primitives::B256,
    log_index: u64,
    block_number: u64,
    block_time: DateTime<Utc>,
    confirmations: Option<u64>,
    asset_contract: EvmAddress,
    amount_atomic: AtomicAmount,
    chain_id: u64,
}

impl From<PendingTransfer> for Observed {
    fn from(transfer: PendingTransfer) -> Self {
        Self {
            status: "seen",
            confirmations: Some(transfer.confirmations()),
            deposit_id: transfer.deposit_id,
            tx_hash: transfer.tx_hash,
            log_index: transfer.log_index,
            block_number: transfer.block_number,
            block_time: transfer.block_time,
            asset_contract: transfer.asset_contract,
            amount_atomic: transfer.amount_atomic,
            chain_id: transfer.chain_id,
        }
    }
}

struct Terms {
    supported: bool,
    in_time: bool,
    amount_within_tolerance: bool,
}

impl Terms {
    fn consumes_lock(&self) -> bool {
        self.supported && self.in_time && self.amount_within_tolerance
    }
}

fn terms(route: &RouteFile, lock: &RateLock, observed: &Observed) -> Terms {
    let open = lock.status != RateLockStatus::Cancelled;
    let supported = observed.chain_id == route.chain.chain_id
        && observed.asset_contract == route.asset.contract;
    Terms {
        supported,
        in_time: open && observed.block_time <= lock.expires_at,
        amount_within_tolerance: open
            && supported
            && amount_within_tolerance(
                observed.amount_atomic,
                lock.amount_atomic,
                route.rate_lock.lock_tolerance_bps,
            ),
    }
}

fn payment(route: &RouteFile, lock: &RateLock, observed: &Observed) -> RateLockPayment {
    let terms = terms(route, lock, observed);
    RateLockPayment {
        status: observed.status.to_owned(),
        deposit_id: observed.deposit_id,
        tx_hash: format!("{:#x}", observed.tx_hash),
        log_index: observed.log_index,
        block_number: observed.block_number,
        confirmations: observed.confirmations,
        amount_atomic: observed.amount_atomic.value().to_string(),
        asset_contract: format!("{:#x}", observed.asset_contract),
        supported: terms.supported,
        amount_within_tolerance: terms.amount_within_tolerance,
        in_time: terms.in_time,
        estimated_final_at: (observed.status == "seen")
            .then(|| estimated_final_at(observed.block_time)),
    }
}

async fn address_deposits(state: &AppState, address_id: Uuid) -> Result<Vec<Observed>, ApiError> {
    let rows = sqlx::query_as::<_, (i64, String, i64, i64, DateTime<Utc>, String, String)>(
        r#"
        SELECT chain_id, tx_hash, log_index, block_number, block_time, asset_contract,
               amount_atomic::text
        FROM deposits
        WHERE address_id = $1
        ORDER BY block_number, log_index
        "#,
    )
    .bind(address_id)
    .fetch_all(&state.pool)
    .await?;
    rows.into_iter()
        .map(
            |(chain_id, tx_hash, log_index, block_number, block_time, asset, amount)| {
                let chain_id = u64::try_from(chain_id).ok()?;
                let tx_hash = tx_hash.parse().ok()?;
                let log_index = u64::try_from(log_index).ok()?;
                Some(Observed {
                    status: "finalized",
                    deposit_id: deposit_id(chain_id, tx_hash, log_index),
                    tx_hash,
                    log_index,
                    block_number: u64::try_from(block_number).ok()?,
                    block_time,
                    confirmations: None,
                    asset_contract: asset.parse().ok()?,
                    amount_atomic: AtomicAmount::new(amount.parse().ok()?),
                    chain_id,
                })
            },
        )
        .collect::<Option<Vec<_>>>()
        .ok_or_else(ApiError::internal)
}

/// Records address activity for the head scan's watched set. Display bookkeeping only, so a
/// failure is logged and never fails the request.
pub(super) async fn touch_requested(state: &AppState, account_id: Uuid) {
    if let Err(error) = db::touch_persistent_requested(&state.pool, account_id).await {
        tracing::warn!(%error, %account_id, "failed to record persistent address activity");
    }
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
