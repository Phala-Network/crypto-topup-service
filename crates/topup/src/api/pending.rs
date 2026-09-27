//! Display-only views of transfers seen before finality (architecture §12).
//!
//! Support, amount matching, and timeliness are computed here at read time and never stored, so
//! an unfinalized transfer can never produce a stored rejection or credit.

use alloy_primitives::Address as EvmAddress;
use chrono::{DateTime, TimeDelta, Utc};
use topup_core::identity::deposit_id;
use topup_core::money::AtomicAmount;
use topup_core::route::RouteFile;
use topup_core::valuation::amount_within_tolerance;
use uuid::Uuid;

use crate::db::{self, PendingTransfer};
use crate::locks::{RateLock, RateLockStatus};

use super::error::ApiError;
use super::models::QuotePayment;
use sqlx::PgPool;

/// Typical Ethereum delay from inclusion to the `finalized` tag: a block in epoch `n` is final
/// once the checkpoint of epoch `n + 1` finalizes, 64 to 95 slots of 12 s (12.8 to 19 minutes).
/// This is an estimate for display; it is not tied to a chain's beacon genesis.
const ESTIMATED_FINALITY_DELAY: TimeDelta = TimeDelta::minutes(15);

/// The payment the quote page shows, following the consumption rule of §9: the deposit that
/// consumed the quote; otherwise the first transfer that would consume it (finalized deposits
/// first, then transfers seen above `finalized`); otherwise the first transfer at all. On a
/// canceled quote no payment matches, because every payment is valued at spot.
pub(super) async fn quote_payment(
    pool: &PgPool,
    route: &RouteFile,
    lock: &RateLock,
) -> Result<Option<QuotePayment>, ApiError> {
    let deposits = address_deposits(pool, lock.address_id).await?;
    if let Some(consumed) = deposits
        .iter()
        .find(|deposit| Some(deposit.deposit_id) == lock.consumed_by)
    {
        return Ok(Some(payment(route, lock, consumed)));
    }
    let observed = deposits
        .into_iter()
        .chain(
            db::list_address_pending(pool, lock.address_id)
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

fn payment(route: &RouteFile, lock: &RateLock, observed: &Observed) -> QuotePayment {
    QuotePayment {
        status: observed.status.to_owned(),
        tx_hash: format!("{:#x}", observed.tx_hash),
        amount_atomic: observed.amount_atomic.value().to_string(),
        confirmations: observed.confirmations,
        estimated_final_at: (observed.status == "seen")
            .then(|| estimated_final_at(observed.block_time).timestamp()),
        matches_quote: terms(route, lock, observed).consumes_lock(),
        deposit: crate::ids::format(crate::ids::DEPOSIT, observed.deposit_id),
    }
}

async fn address_deposits(pool: &PgPool, address_id: Uuid) -> Result<Vec<Observed>, ApiError> {
    let rows = sqlx::query_as::<_, (i64, String, i64, DateTime<Utc>, String, String)>(
        r#"
        SELECT chain_id, tx_hash, log_index, block_time, asset_contract, amount_atomic::text
        FROM deposits
        WHERE address_id = $1
        ORDER BY block_number, log_index
        "#,
    )
    .bind(address_id)
    .fetch_all(pool)
    .await?;
    rows.into_iter()
        .map(
            |(chain_id, tx_hash, log_index, block_time, asset, amount)| {
                let chain_id = u64::try_from(chain_id).ok()?;
                let tx_hash = tx_hash.parse().ok()?;
                let log_index = u64::try_from(log_index).ok()?;
                Some(Observed {
                    status: "final",
                    deposit_id: deposit_id(chain_id, tx_hash, log_index),
                    tx_hash,
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

fn estimated_final_at(block_time: DateTime<Utc>) -> DateTime<Utc> {
    block_time
        .checked_add_signed(ESTIMATED_FINALITY_DELAY)
        .unwrap_or(block_time)
}
