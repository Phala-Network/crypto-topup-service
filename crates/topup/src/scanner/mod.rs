//! Finalized ERC-20 deposit scanner.

mod head;

pub use head::{HEAD_SCAN_INTERVAL, HeadScan, head_scan_once};

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::future::Future;
use std::time::Duration;

use alloy_primitives::Address;
use chrono::Utc;
use sqlx::PgPool;
use tokio::sync::Notify;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::{
    ChainError, ChainReader, EvmChain, MAX_ADDRESSES_PER_REQUEST, MAX_BLOCKS_PER_REQUEST,
    TransferLog,
};
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::retry::backoff;
use topup_core::route::{ChainConfig, RouteFile};
use tracing::Instrument as _;

use crate::db::{self, NewDeposit, ScanAddress, ScanCommit};
use crate::jitter::{JitterSource as _, OsJitter};
use crate::rpc_provider::{configured_provider_url, provider_label};

/// Maximum inclusive block count scanned in one window.
pub const MAX_SCAN_WINDOW: u64 = MAX_BLOCKS_PER_REQUEST;

/// Scanner failure.
#[derive(Debug)]
pub enum ScannerError {
    /// A route file or environment value is invalid.
    Configuration(String),
    /// A chain read failed.
    Chain(ChainError),
    /// A database operation failed.
    Database(sqlx::Error),
    /// The provider finalized head is behind the durable cursor.
    FinalizedBehindCursor {
        /// Durable fully scanned block.
        cursor: u64,
        /// Provider's current finalized block.
        finalized: u64,
    },
    /// A transfer returned for the filter did not resolve to a tracked address.
    UnknownRecipient(Address),
    /// A per-chain scanner task stopped unexpectedly.
    Task(String),
}

impl Display for ScannerError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) => {
                write!(formatter, "invalid scanner configuration: {message}")
            }
            Self::Chain(error) => Display::fmt(error, formatter),
            Self::Database(error) => Display::fmt(error, formatter),
            Self::FinalizedBehindCursor { cursor, finalized } => write!(
                formatter,
                "provider finalized head {finalized} is behind durable cursor {cursor}"
            ),
            Self::UnknownRecipient(address) => {
                write!(formatter, "transfer recipient {address:#x} is not tracked")
            }
            Self::Task(message) => write!(formatter, "scanner task failed: {message}"),
        }
    }
}

impl Error for ScannerError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Chain(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Configuration(_)
            | Self::FinalizedBehindCursor { .. }
            | Self::UnknownRecipient(_)
            | Self::Task(_) => None,
        }
    }
}

impl From<ChainError> for ScannerError {
    fn from(error: ChainError) -> Self {
        Self::Chain(error)
    }
}

impl From<sqlx::Error> for ScannerError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl ScannerError {
    fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::Database(_)
                | Self::Chain(
                    ChainError::Rpc(_)
                        | ChainError::Transport(_)
                        | ChainError::MissingField(_)
                        | ChainError::InvalidTimestamp(_)
                        | ChainError::InvalidTransfer(_)
                )
        )
    }

    fn category(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Chain(ChainError::FinalizedHeadRegressed { .. }) => "finalized_regression",
            Self::Chain(ChainError::ProviderUnhealthy) => "provider_unhealthy",
            Self::Chain(_) => "chain_read",
            Self::Database(_) => "database",
            Self::FinalizedBehindCursor { .. } => "finalized_behind_cursor",
            Self::UnknownRecipient(_) => "unknown_recipient",
            Self::Task(_) => "task",
        }
    }
}

/// Active routes and chain settings for one EVM chain.
#[derive(Clone, Debug)]
pub struct ChainRoutes {
    /// Shared chain configuration.
    pub chain: ChainConfig,
    routes: BTreeMap<Address, RouteSelection>,
}

#[derive(Clone, Debug)]
struct RouteSelection {
    name: String,
    version: u64,
}

/// Work completed by one polling pass.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ScanStats {
    /// Deposits newly inserted; duplicate chain logs are excluded.
    pub inserted: u64,
    /// Highest cursor committed during the pass.
    pub cursor: u64,
    /// Finalized head observed during the pass.
    pub finalized: u64,
    /// Addresses whose one-time historical backfill completed.
    pub backfilled_addresses: u64,
}

impl ScanStats {
    fn record_inserted(&mut self, inserted: u64) -> Result<(), ScannerError> {
        self.inserted = self
            .inserted
            .checked_add(inserted)
            .ok_or_else(|| ScannerError::Configuration("scan count overflow".to_owned()))?;
        Ok(())
    }

    fn record_backfilled(&mut self, count: usize) -> Result<(), ScannerError> {
        let count = u64::try_from(count).map_err(|error| {
            ScannerError::Configuration(format!("backfill count is outside u64: {error}"))
        })?;
        self.backfilled_addresses = self
            .backfilled_addresses
            .checked_add(count)
            .ok_or_else(|| ScannerError::Configuration("backfill count overflow".to_owned()))?;
        Ok(())
    }
}

/// Selects the highest supplied route version for each chain and asset.
pub fn configure_routes(routes: &[RouteFile]) -> Result<Vec<ChainRoutes>, ScannerError> {
    if routes.is_empty() {
        return Err(ScannerError::Configuration(
            "at least one route is required".to_owned(),
        ));
    }
    let mut chains = BTreeMap::<u64, ChainRoutes>::new();
    for route in routes {
        route.validate().map_err(|error| {
            ScannerError::Configuration(format!(
                "route `{}` version {} failed validation: {error}",
                route.route, route.version
            ))
        })?;
        if route.chain.finality != "finalized" {
            return Err(ScannerError::Configuration(format!(
                "route `{}` version {} uses unsupported finality rule `{}`",
                route.route, route.version, route.chain.finality
            )));
        }

        let chain_id = route.chain.chain_id;
        let chain_routes = chains.entry(chain_id).or_insert_with(|| ChainRoutes {
            chain: route.chain.clone(),
            routes: BTreeMap::new(),
        });
        if chain_routes.chain.finality != route.chain.finality
            || chain_routes.chain.rpc_providers != route.chain.rpc_providers
        {
            return Err(ScannerError::Configuration(format!(
                "route `{}` version {} disagrees with another chain {chain_id} scanner configuration",
                route.route, route.version
            )));
        }
        let selection = RouteSelection {
            name: route.route.clone(),
            version: route.version,
        };
        match chain_routes.routes.get_mut(&route.asset.contract) {
            Some(current) if current.name != selection.name => {
                return Err(ScannerError::Configuration(format!(
                    "routes `{}` and `{}` both select chain {chain_id} asset {:#x}",
                    current.name, selection.name, route.asset.contract
                )));
            }
            Some(current) if selection.version > current.version => *current = selection,
            Some(_) => {}
            None => {
                chain_routes.routes.insert(route.asset.contract, selection);
            }
        }
    }
    Ok(chains.into_values().collect())
}

/// Scans one chain through its current finalized head and commits durable progress.
pub async fn scan_once<R: ChainReader>(
    pool: &PgPool,
    reader: &R,
    routes: &ChainRoutes,
) -> Result<ScanStats, ScannerError> {
    let chain_id = routes.chain.chain_id;
    let cursor = db::get_cursor(pool, chain_id).await?.unwrap_or(0);
    if crate::reconciler::chain_is_blocked(pool, chain_id).await? {
        tracing::warn!(
            chain_id,
            "finalized chain scan paused because reconciliation froze the chain"
        );
        return Ok(ScanStats {
            cursor,
            ..ScanStats::default()
        });
    }
    let head = reader.finalized_head().await?;
    let finalized = head.number;
    if finalized < cursor {
        return Err(ScannerError::FinalizedBehindCursor { cursor, finalized });
    }
    let addresses = db::list_scan_addresses(pool, chain_id).await?;
    let address_index = address_index(&addresses);
    let mut stats = ScanStats {
        cursor,
        finalized,
        ..ScanStats::default()
    };

    let pending_backfills = addresses
        .iter()
        .filter(|address| !address.backfilled && address.created_block <= cursor)
        .cloned()
        .collect::<Vec<_>>();
    for address in &pending_backfills {
        for (from_block, to_block) in scan_windows(address.created_block, cursor.min(finalized))? {
            let span = crate::observability::scanner_window_span(chain_id, from_block, to_block);
            let committed = async {
                let logs = reader
                    .transfer_logs_to(&[address.address], from_block, to_block)
                    .await?;
                let deposits = resolve_logs(logs, &address_index, routes)?;
                db::commit_scan(pool, chain_id, &deposits, &[], None, None)
                    .await
                    .map_err(ScannerError::from)
            }
            .instrument(span)
            .await?;
            record_committed(chain_id, &mut stats, committed)?;
        }
        db::commit_scan(pool, chain_id, &[], &[address.id], None, None).await?;
        stats.record_backfilled(1)?;
    }

    let tracked = addresses
        .iter()
        .map(|address| address.address)
        .collect::<Vec<_>>();
    let mut pending_backfill_marks = addresses
        .iter()
        .filter(|address| !address.backfilled)
        .map(|address| (address.id, address.created_block))
        .collect::<BTreeMap<_, _>>();
    for address in &pending_backfills {
        pending_backfill_marks.remove(&address.id);
    }
    let Some(start) = cursor.checked_add(1) else {
        return Ok(stats);
    };
    if start > finalized {
        return Ok(stats);
    }

    for (from_block, to_block) in scan_windows(start, finalized)? {
        for batch in tracked.chunks(MAX_ADDRESSES_PER_REQUEST) {
            let span = crate::observability::scanner_window_span(chain_id, from_block, to_block);
            let committed = async {
                let logs = reader.transfer_logs_to(batch, from_block, to_block).await?;
                let deposits = resolve_logs(logs, &address_index, routes)?;
                db::commit_scan(pool, chain_id, &deposits, &[], None, None)
                    .await
                    .map_err(ScannerError::from)
            }
            .instrument(span)
            .await?;
            record_committed(chain_id, &mut stats, committed)?;
        }
        let backfilled = pending_backfill_marks
            .iter()
            .filter(|(_, created_block)| **created_block <= to_block)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        // Only the finalized head's time is known without another RPC; intermediate windows keep
        // the previous time, a lower bound on the cursor block's time.
        let scanned_block_time = (to_block == finalized).then_some(head.time);
        db::commit_scan(
            pool,
            chain_id,
            &[],
            &backfilled,
            Some(to_block),
            scanned_block_time,
        )
        .await?;
        stats.record_backfilled(backfilled.len())?;
        for id in &backfilled {
            pending_backfill_marks.remove(id);
        }
        stats.cursor = to_block;
    }
    Ok(stats)
}

/// Runs every configured chain scanner until cancellation or all chains stop.
pub async fn run(
    pool: PgPool,
    chains: Vec<ChainRoutes>,
    cancellation: CancellationToken,
) -> Result<(), ScannerError> {
    let poll_interval = poll_interval_from_env()?;
    let mut tasks = JoinSet::new();
    for routes in chains {
        let provider_id = routes
            .chain
            .rpc_providers
            .first()
            .ok_or_else(|| ScannerError::Configuration("chain has no provider A".to_owned()))?;
        let rpc_url = configured_provider_url(provider_id).map_err(|environment| {
            ScannerError::Configuration(format!("{environment} is required for provider A"))
        })?;
        let rpc_url = crate::observability::Redacted::parse(&rpc_url).map_err(|_| {
            ScannerError::Configuration(format!(
                "provider `{}` does not contain a valid URL",
                provider_label(provider_id, 0)
            ))
        })?;
        let reader =
            EvmChain::new(rpc_url.expose().as_str())?.with_provider(provider_label(provider_id, 0));
        let chain_pool = pool.clone();
        let chain_cancellation = cancellation.child_token();
        tasks.spawn(async move {
            run_chain(
                chain_pool,
                reader,
                routes,
                poll_interval,
                chain_cancellation,
            )
            .await
        });
    }

    if tasks.is_empty() {
        return Err(ScannerError::Configuration(
            "no chain scanners were configured".to_owned(),
        ));
    }
    let mut first_error = None;
    while let Some(result) = tasks.join_next().await {
        match result {
            Ok(Err(error)) => {
                tracing::error!(
                    error_category = error.category(),
                    "chain scanner task stopped"
                );
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
            Err(error) => {
                tracing::error!(error = %error, "chain scanner task failed to join");
                if first_error.is_none() {
                    first_error = Some(ScannerError::Task(error.to_string()));
                }
            }
            Ok(Ok(())) if cancellation.is_cancelled() => {}
            Ok(Ok(())) => {
                tracing::error!("chain scanner task exited unexpectedly");
                if first_error.is_none() {
                    first_error = Some(ScannerError::Task(
                        "chain scanner exited unexpectedly".to_owned(),
                    ));
                }
            }
        }
    }
    if let Some(error) = first_error {
        Err(error)
    } else if cancellation.is_cancelled() {
        Ok(())
    } else {
        Err(ScannerError::Task(
            "all chain scanner tasks stopped".to_owned(),
        ))
    }
}

async fn run_chain(
    pool: PgPool,
    reader: EvmChain,
    routes: ChainRoutes,
    poll_interval: Duration,
    cancellation: CancellationToken,
) -> Result<(), ScannerError> {
    let chain_id = routes.chain.chain_id;
    let finalized_advanced = Notify::new();
    let finalized_scan = run_scan_loop(
        chain_id,
        poll_interval,
        cancellation.clone(),
        || scan_once(&pool, &reader, &routes),
        |delay| {
            let finalized_advanced = &finalized_advanced;
            async move {
                tokio::select! {
                    () = tokio::time::sleep(delay) => {}
                    () = finalized_advanced.notified() => {}
                }
            }
        },
        || OsJitter.next_u64(),
    );
    let head_scan = head::run_head_loop(
        &pool,
        &reader,
        &routes,
        poll_interval.min(HEAD_SCAN_INTERVAL),
        &finalized_advanced,
        cancellation,
    );
    // The head loop returns only on cancellation; the finalized loop's result is the chain's.
    tokio::select! {
        result = finalized_scan => result,
        () = head_scan => Ok(()),
    }
}

async fn run_scan_loop<Scan, ScanFuture, Sleep, SleepFuture, Jitter>(
    chain_id: u64,
    poll_interval: Duration,
    cancellation: CancellationToken,
    mut scan: Scan,
    mut sleep: Sleep,
    mut jitter: Jitter,
) -> Result<(), ScannerError>
where
    Scan: FnMut() -> ScanFuture,
    ScanFuture: Future<Output = Result<ScanStats, ScannerError>>,
    Sleep: FnMut(Duration) -> SleepFuture,
    SleepFuture: Future<Output = ()>,
    Jitter: FnMut() -> u64,
{
    let mut retry_attempt = 0_u32;
    let mut last_success = std::time::Instant::now();
    let mut last_progress = (0_u64, 0_u64);
    let instance = chain_id.to_string();
    crate::observability::register_scanner(chain_id);
    loop {
        crate::observability::heartbeat("scanner", instance.clone());
        let result = tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            result = scan() => result,
        };
        let delay = match result {
            Ok(stats) => {
                retry_attempt = 0;
                last_success = std::time::Instant::now();
                let made_progress = stats.cursor > last_progress.1
                    || stats.inserted > 0
                    || stats.backfilled_addresses > 0;
                last_progress = (stats.finalized, stats.cursor);
                crate::observability::record_scanner_lag(
                    chain_id,
                    stats.finalized,
                    stats.cursor,
                    0,
                );
                crate::observability::record_scanner_success(chain_id);
                if made_progress {
                    crate::observability::progress("scanner", instance.clone());
                }
                tracing::info!(
                    chain_id,
                    cursor = stats.cursor,
                    inserted = stats.inserted,
                    backfilled_addresses = stats.backfilled_addresses,
                    "finalized chain scan committed"
                );
                poll_interval
            }
            Err(error) if error.is_retryable() => {
                crate::observability::record_scanner_lag(
                    chain_id,
                    last_progress.0,
                    last_progress.1,
                    last_success.elapsed().as_secs(),
                );
                let delay = backoff(retry_attempt, jitter());
                retry_attempt = retry_attempt.saturating_add(1);
                tracing::warn!(
                    chain_id,
                    error_category = error.category(),
                    %error,
                    retry_after_seconds = delay.as_secs(),
                    "finalized chain scan failed transiently"
                );
                delay
            }
            Err(error) => {
                tracing::error!(
                    chain_id,
                    error_category = error.category(),
                    %error,
                    "finalized chain scanner stopped"
                );
                return Err(error);
            }
        };
        crate::observability::waiting("scanner", instance.clone(), delay);
        tokio::select! {
            () = cancellation.cancelled() => return Ok(()),
            () = sleep(delay) => {}
        }
    }
}

fn record_committed(
    chain_id: u64,
    stats: &mut ScanStats,
    committed: ScanCommit,
) -> Result<(), ScannerError> {
    stats.record_inserted(committed.inserted)?;
    if committed.unsupported_inserted > 0 {
        metrics::counter!(
            "topup_unsupported_inflows_total",
            "chain" => chain_id.to_string(),
            "producer_enabled" => "true",
        )
        .increment(committed.unsupported_inserted);
    }
    Ok(())
}

fn poll_interval_from_env() -> Result<Duration, ScannerError> {
    let seconds = match std::env::var("TOPUP_SCANNER_POLL_INTERVAL_SECONDS") {
        Ok(value) => value.parse::<u64>().map_err(|error| {
            ScannerError::Configuration(format!(
                "TOPUP_SCANNER_POLL_INTERVAL_SECONDS must be an integer: {error}"
            ))
        })?,
        Err(std::env::VarError::NotPresent) => 15,
        Err(error) => {
            return Err(ScannerError::Configuration(format!(
                "TOPUP_SCANNER_POLL_INTERVAL_SECONDS is invalid: {error}"
            )));
        }
    };
    if seconds == 0 {
        return Err(ScannerError::Configuration(
            "TOPUP_SCANNER_POLL_INTERVAL_SECONDS must be positive".to_owned(),
        ));
    }
    Ok(Duration::from_secs(seconds))
}

fn address_index(addresses: &[ScanAddress]) -> BTreeMap<Address, ScanAddress> {
    addresses
        .iter()
        .cloned()
        .map(|address| (address.address, address))
        .collect()
}

pub(crate) fn resolve_logs_for_reconciliation(
    logs: Vec<TransferLog>,
    addresses: &[ScanAddress],
    routes: &ChainRoutes,
) -> Result<Vec<NewDeposit>, ScannerError> {
    resolve_logs(logs, &address_index(addresses), routes)
}

fn resolve_logs(
    logs: Vec<TransferLog>,
    addresses: &BTreeMap<Address, ScanAddress>,
    routes: &ChainRoutes,
) -> Result<Vec<NewDeposit>, ScannerError> {
    let next_attempt_at = Utc::now();
    logs.into_iter()
        .map(|log| {
            let address = addresses
                .get(&log.to)
                .ok_or(ScannerError::UnknownRecipient(log.to))?;
            let selected = routes.routes.get(&log.token);
            Ok(NewDeposit {
                chain_id: routes.chain.chain_id,
                tx_hash: log.tx_hash,
                log_index: log.log_index,
                block_number: log.block_number,
                block_hash: log.block_hash,
                block_time: log.block_time,
                address_id: address.id,
                account_id: address.account_id,
                route: selected.map(|route| route.name.clone()),
                route_version: selected.map(|route| route.version),
                asset_contract: log.token,
                from_address: log.from,
                amount_atomic: log.amount,
                state: if selected.is_some() {
                    DepositState::Detected
                } else {
                    DepositState::Rejected
                },
                reason: selected.is_none().then_some(RejectReason::UnsupportedAsset),
                next_attempt_at,
            })
        })
        .collect()
}

fn scan_windows(from_block: u64, to_block: u64) -> Result<Vec<(u64, u64)>, ScannerError> {
    if from_block > to_block {
        return Err(ScannerError::Configuration(format!(
            "scan range starts at {from_block} after {to_block}"
        )));
    }
    let mut windows = Vec::new();
    let mut start = from_block;
    loop {
        let end = start
            .saturating_add(MAX_SCAN_WINDOW.saturating_sub(1))
            .min(to_block);
        windows.push((start, end));
        if end == to_block {
            break;
        }
        start = end
            .checked_add(1)
            .ok_or_else(|| ScannerError::Configuration("scan block range overflow".to_owned()))?;
    }
    Ok(windows)
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::future;
    use std::sync::Mutex;

    use alloy_primitives::{B256, U256};
    use chrono::DateTime;
    use topup_core::money::AtomicAmount;
    use uuid::Uuid;

    use super::*;

    #[test]
    fn scanner_windows_are_inclusive_and_bounded() {
        assert_eq!(
            scan_windows(1, 4_001).expect("valid range"),
            vec![(1, 2_000), (2_001, 4_000), (4_001, 4_001)]
        );
    }

    #[tokio::test]
    async fn scan_loop_recovers_after_a_transient_failure() {
        let results = Mutex::new(VecDeque::from([
            Err(ScannerError::Database(sqlx::Error::PoolTimedOut)),
            Ok(ScanStats {
                inserted: 1,
                cursor: 2,
                finalized: 2,
                backfilled_addresses: 0,
            }),
            Err(ScannerError::Chain(ChainError::ProviderUnhealthy)),
        ]));
        let sleeps = Mutex::new(Vec::new());

        let error = run_scan_loop(
            1,
            Duration::from_secs(15),
            CancellationToken::new(),
            || {
                future::ready(
                    results
                        .lock()
                        .expect("results lock")
                        .pop_front()
                        .expect("scripted scan result"),
                )
            },
            |duration| {
                sleeps.lock().expect("sleeps lock").push(duration);
                future::ready(())
            },
            || u64::MAX,
        )
        .await
        .expect_err("provider health violation must stop the loop");

        assert!(matches!(
            error,
            ScannerError::Chain(ChainError::ProviderUnhealthy)
        ));
        assert_eq!(
            *sleeps.lock().expect("sleeps lock"),
            vec![Duration::ZERO, Duration::from_secs(15)]
        );
        assert!(results.lock().expect("results lock").is_empty());
    }

    #[test]
    fn highest_route_version_is_current_for_an_asset() {
        let token = Address::from([7_u8; 20]);
        let mut older = test_route_file(token);
        older.version = 1;
        let mut newer = older.clone();
        newer.version = 2;

        let chains = configure_routes(&[newer, older]).expect("versioned routes");
        let chain = chains.first().expect("one chain");
        let selected = chain.routes.get(&token).expect("selected route");

        assert_eq!(selected.version, 2);
    }

    #[test]
    fn unsupported_asset_is_born_rejected() {
        let recipient = Address::from([1_u8; 20]);
        let address = ScanAddress {
            id: Uuid::new_v4(),
            account_id: Uuid::new_v4(),
            address: recipient,
            created_block: 0,
            backfilled: false,
        };
        let routes = test_routes(Address::from([2_u8; 20]));
        let log = TransferLog {
            tx_hash: B256::from([3_u8; 32]),
            log_index: 0,
            block_number: 1,
            block_hash: B256::from([4_u8; 32]),
            block_time: DateTime::from_timestamp(1, 0).expect("timestamp"),
            token: Address::from([5_u8; 20]),
            from: Address::from([6_u8; 20]),
            to: recipient,
            amount: AtomicAmount::new(U256::from(7)),
        };
        let deposits =
            resolve_logs(vec![log], &address_index(&[address]), &routes).expect("resolve log");
        let deposit = deposits.first().expect("one deposit");
        assert_eq!(deposit.state, DepositState::Rejected);
        assert_eq!(deposit.reason, Some(RejectReason::UnsupportedAsset));
        assert_eq!(deposit.route, None);
    }

    fn test_routes(token: Address) -> ChainRoutes {
        let route = test_route_file(token);
        ChainRoutes {
            chain: route.chain,
            routes: BTreeMap::from([(
                token,
                RouteSelection {
                    name: route.route,
                    version: route.version,
                },
            )]),
        }
    }

    fn test_route_file(token: Address) -> RouteFile {
        let yaml = include_str!("../../tests/fixtures/phala-cloud-pha.yaml").replace(
            "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E",
            &format!("{token:#x}"),
        );
        serde_saphyr::from_str(&yaml).expect("route fixture")
    }
}
