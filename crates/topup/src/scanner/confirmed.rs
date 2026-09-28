//! Fast scan at the route's confirmation (design D1, architecture §8): transfers to watched
//! addresses (open quotes, until an hour after expiry, and every deposit address) in blocks that reached the route's depth or
//! `safe` head on provider A become `detected` deposits at once, and the pump confirms them on both
//! providers.
//!
//! The scan covers `(max(finalized cursor, fast cursor), horizon]`, at most one scan window below
//! the horizon. A transfer it does not record (to an address no quote or deposit address watches, in a block it
//! skipped, or introduced below its cursor by a reorg deeper than the confirmation) is recorded
//! by the finalized scanner, whose insert is keyed by the same identity.

use sqlx::PgPool;
use topup_adapters::chain::evm::{ChainReader, MAX_ADDRESSES_PER_REQUEST};
use topup_core::route::Confirmations;

use super::{ChainRoutes, MAX_SCAN_WINDOW, ScannerError, address_index, resolve_logs};
use crate::db;

/// Outcome of one fast scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ConfirmedScan {
    /// Highest block at the route's confirmation on provider A.
    pub horizon: u64,
    /// Deposits newly recorded.
    pub inserted: u64,
}

/// Records transfers that reached the route's confirmation on provider A since the last scan.
///
/// Returns `None` when the route credits only at `finalized`, while reconciliation has frozen the
/// chain, and before the finalized scanner's first commit.
pub async fn confirmed_scan_once<R: ChainReader>(
    pool: &PgPool,
    reader: &R,
    routes: &ChainRoutes,
) -> Result<Option<ConfirmedScan>, ScannerError> {
    let chain_id = routes.chain.chain_id;
    let confirmations = routes.chain.confirmations;
    if confirmations == Confirmations::Finalized
        || crate::reconciler::chain_is_blocked(pool, chain_id).await?
    {
        return Ok(None);
    }
    let Some(finalized_cursor) = db::get_cursor(pool, chain_id).await? else {
        return Ok(None);
    };
    let heads = reader.confirmation_heads(confirmations).await?;
    let horizon = confirmations.horizon(heads);
    let scanned = db::get_confirmed_cursor(pool, chain_id)
        .await?
        .unwrap_or(0)
        .max(finalized_cursor);
    let from_block = scanned
        .saturating_add(1)
        .max(horizon.saturating_sub(MAX_SCAN_WINDOW.saturating_sub(1)));
    if from_block > horizon {
        return Ok(Some(ConfirmedScan {
            horizon,
            inserted: 0,
        }));
    }
    let addresses = db::list_watched_addresses(pool, chain_id).await?;
    let index = address_index(&addresses);
    let tracked = addresses
        .iter()
        .map(|address| address.address)
        .collect::<Vec<_>>();
    let mut deposits = Vec::new();
    for batch in tracked.chunks(MAX_ADDRESSES_PER_REQUEST) {
        let logs = reader.transfer_logs_to(batch, from_block, horizon).await?;
        deposits.extend(resolve_logs(logs, &index, routes)?);
    }
    let committed = db::commit_confirmed_scan(pool, chain_id, &deposits, horizon).await?;
    super::record_committed(chain_id, &mut super::ScanStats::default(), committed)?;
    Ok(Some(ConfirmedScan {
        horizon,
        inserted: committed.inserted,
    }))
}
