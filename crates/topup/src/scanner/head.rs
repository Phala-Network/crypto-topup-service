//! Display-only head scan (architecture §8): transfers to watched addresses in
//! `[finalized + 1, latest]` on provider A, stored in `pending_transfers` so products can show
//! "received, N confirmations" before finality. Nothing here can create or change a deposit.

use std::collections::BTreeMap;
use std::time::Duration;

use sqlx::PgPool;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::{ChainReader as _, FinalizedReader};

use super::ChainRoutes;

use super::{MAX_SCAN_WINDOW, ScannerError};
use crate::db::{self, HeadCommit, NewPendingTransfer};

/// Longest interval between head scans: about one Ethereum slot. A shorter scanner poll interval
/// (`--scanner-poll-interval-s`) also shortens the head scan.
pub const HEAD_SCAN_INTERVAL: Duration = Duration::from_secs(12);

/// Outcome of one head scan.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct HeadScan {
    /// Provider A finalized head observed by the scan.
    pub finalized: u64,
    /// Provider A latest head observed by the scan.
    pub latest: u64,
    /// Rows written and removed.
    pub commit: HeadCommit,
}

/// Scans `[finalized + 1, latest]` (at most one scan window below `latest`) for non-zero transfers
/// of the chain's routed tokens to watched addresses and replaces the stored pending view of that
/// range. Other tokens are not requested, so they cannot create pending rows or notifications;
/// they become visible after finality as `rejected(unsupported_asset)`. Returns `None` while
/// reconciliation has frozen the chain.
pub async fn head_scan_once(
    pool: &PgPool,
    reader: &FinalizedReader,
    routes: &ChainRoutes,
) -> Result<Option<HeadScan>, ScannerError> {
    let chain_id = routes.chain.chain_id;
    if crate::reconciler::chain_is_blocked(pool, chain_id).await? {
        return Ok(None);
    }
    let finalized = reader.finalized_head().await?.number;
    let latest = reader.latest_head().await?;
    let from_block = finalized
        .saturating_add(1)
        .max(latest.saturating_sub(MAX_SCAN_WINDOW.saturating_sub(1)));
    let watched = db::list_watched_addresses(pool, chain_id).await?;
    let mut transfers = Vec::new();
    if from_block <= latest && !watched.is_empty() {
        let index = watched
            .iter()
            .map(|address| (address.address, address.id))
            .collect::<BTreeMap<_, _>>();
        let addresses = index.keys().copied().collect::<Vec<_>>();
        let tokens = routes.routes.keys().copied().collect::<Vec<_>>();
        for log in reader
            .token_transfer_logs_to(&tokens, &addresses, from_block, latest)
            .await?
        {
            if log.amount.value().is_zero() {
                continue;
            }
            let address_id = *index
                .get(&log.to)
                .ok_or(ScannerError::UnknownRecipient(log.to))?;
            transfers.push(NewPendingTransfer {
                chain_id,
                tx_hash: log.tx_hash,
                log_index: log.log_index,
                block_number: log.block_number,
                block_hash: log.block_hash,
                block_time: log.block_time,
                address_id,
                asset_contract: log.token,
                from_address: log.from,
                amount_atomic: log.amount,
            });
        }
    }
    let commit = db::commit_head_scan(pool, chain_id, from_block, latest, &transfers).await?;
    Ok(Some(HeadScan {
        finalized,
        latest,
        commit,
    }))
}

/// Runs the head scan every `interval` until cancellation. Failures only delay the
/// pending view, so they are logged and retried. When provider A's `finalized` advances, the
/// finalized scanner is woken instead of waiting for its poll interval.
pub(super) async fn run_head_loop(
    pool: &PgPool,
    reader: &FinalizedReader,
    routes: &ChainRoutes,
    interval: Duration,
    finalized_advanced: &Notify,
    cancellation: CancellationToken,
) {
    let chain_id = routes.chain.chain_id;
    let mut last_finalized = None;
    loop {
        let result = tokio::select! {
            () = cancellation.cancelled() => return,
            result = head_scan_once(pool, reader, routes) => result,
        };
        match result {
            Ok(Some(scan)) => {
                if last_finalized.is_some_and(|last| scan.finalized > last) {
                    finalized_advanced.notify_one();
                }
                last_finalized = Some(scan.finalized);
                tracing::debug!(
                    chain_id,
                    finalized = scan.finalized,
                    latest = scan.latest,
                    seen = scan.commit.seen,
                    removed = scan.commit.removed,
                    "head scan committed"
                );
            }
            Ok(None) => {}
            Err(error) => tracing::warn!(
                chain_id,
                error_category = error.category(),
                %error,
                "display-only head scan failed; retrying"
            ),
        }
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = tokio::time::sleep(interval) => {}
        }
    }
}
