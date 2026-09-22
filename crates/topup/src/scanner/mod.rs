//! Finalized ERC-20 deposit scanner.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;
use std::time::Duration;

use alloy_primitives::Address;
use chrono::Utc;
use sqlx::PgPool;
use tokio::task::JoinSet;
use topup_adapters::chain::evm::{ChainError, ChainReader, EvmChain, TransferLog};
use topup_core::deposit::{DepositState, RejectReason};
use topup_core::route::{ChainConfig, RouteFile};

use crate::db::{self, NewDeposit, ScanAddress};

/// Maximum inclusive block count committed by one scanner transaction.
pub const MAX_SCAN_WINDOW: u64 = 2_000;

/// Scanner failure.
#[derive(Debug)]
pub enum ScannerError {
    /// A route file or environment value is invalid.
    Configuration(String),
    /// A route file could not be read.
    Io(std::io::Error),
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
            Self::Io(error) => write!(formatter, "failed to read scanner configuration: {error}"),
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
            Self::Io(error) => Some(error),
            Self::Chain(error) => Some(error),
            Self::Database(error) => Some(error),
            Self::Configuration(_)
            | Self::FinalizedBehindCursor { .. }
            | Self::UnknownRecipient(_)
            | Self::Task(_) => None,
        }
    }
}

impl From<std::io::Error> for ScannerError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
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
    /// Addresses whose one-time historical backfill completed.
    pub backfilled_addresses: u64,
}

/// Loads and validates route files, grouped by chain identifier.
pub fn load_route_files(paths: &[PathBuf]) -> Result<Vec<ChainRoutes>, ScannerError> {
    if paths.is_empty() {
        return Err(ScannerError::Configuration(
            "TOPUP_ROUTE_FILES must contain at least one path".to_owned(),
        ));
    }
    let mut chains = BTreeMap::<u64, ChainRoutes>::new();
    for path in paths {
        let yaml = std::fs::read_to_string(path)?;
        let route: RouteFile = serde_saphyr::from_str(&yaml).map_err(|error| {
            ScannerError::Configuration(format!(
                "route file `{}` is invalid YAML: {error}",
                path.display()
            ))
        })?;
        route.validate().map_err(|error| {
            ScannerError::Configuration(format!(
                "route file `{}` failed validation: {error}",
                path.display()
            ))
        })?;
        if route.chain.finality != "finalized" {
            return Err(ScannerError::Configuration(format!(
                "route file `{}` uses unsupported finality rule `{}`",
                path.display(),
                route.chain.finality
            )));
        }

        let chain_id = route.chain.chain_id;
        let chain_routes = chains.entry(chain_id).or_insert_with(|| ChainRoutes {
            chain: route.chain.clone(),
            routes: BTreeMap::new(),
        });
        if chain_routes.chain != route.chain {
            return Err(ScannerError::Configuration(format!(
                "route file `{}` disagrees with another chain {chain_id} configuration",
                path.display()
            )));
        }
        if chain_routes
            .routes
            .insert(
                route.asset.contract,
                RouteSelection {
                    name: route.route,
                    version: route.version,
                },
            )
            .is_some()
        {
            return Err(ScannerError::Configuration(format!(
                "multiple active routes select chain {chain_id} asset {:#x}",
                route.asset.contract
            )));
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
    let finalized = reader.finalized_head().await?;
    let cursor = db::get_cursor(pool, chain_id).await?.unwrap_or(0);
    if finalized < cursor {
        return Err(ScannerError::FinalizedBehindCursor { cursor, finalized });
    }
    let addresses = db::list_scan_addresses(pool, chain_id).await?;
    let address_index = address_index(&addresses);
    let mut stats = ScanStats {
        cursor,
        ..ScanStats::default()
    };

    let pending_backfills = addresses
        .iter()
        .filter(|address| !address.backfilled && address.created_block <= cursor)
        .cloned()
        .collect::<Vec<_>>();
    if !pending_backfills.is_empty() {
        let mut deposits = Vec::new();
        for address in &pending_backfills {
            let logs = reader
                .transfer_logs_to(
                    &[address.address],
                    address.created_block,
                    cursor.min(finalized),
                )
                .await?;
            deposits.extend(resolve_logs(logs, &address_index, routes)?);
        }
        let ids = pending_backfills
            .iter()
            .map(|address| address.id)
            .collect::<Vec<_>>();
        let committed = db::commit_scan(pool, chain_id, &deposits, &ids, None).await?;
        stats.inserted = stats
            .inserted
            .checked_add(committed.inserted)
            .ok_or_else(|| ScannerError::Configuration("scan count overflow".to_owned()))?;
        stats.backfilled_addresses = u64::try_from(ids.len()).map_err(|error| {
            ScannerError::Configuration(format!("backfill count is outside u64: {error}"))
        })?;
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
        let logs = reader
            .transfer_logs_to(&tracked, from_block, to_block)
            .await?;
        let deposits = resolve_logs(logs, &address_index, routes)?;
        let backfilled = pending_backfill_marks
            .iter()
            .filter(|(_, created_block)| **created_block <= to_block)
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        let committed =
            db::commit_scan(pool, chain_id, &deposits, &backfilled, Some(to_block)).await?;
        stats.inserted = stats
            .inserted
            .checked_add(committed.inserted)
            .ok_or_else(|| ScannerError::Configuration("scan count overflow".to_owned()))?;
        stats.backfilled_addresses = stats
            .backfilled_addresses
            .checked_add(u64::try_from(backfilled.len()).map_err(|error| {
                ScannerError::Configuration(format!("backfill count is outside u64: {error}"))
            })?)
            .ok_or_else(|| ScannerError::Configuration("backfill count overflow".to_owned()))?;
        for id in &backfilled {
            pending_backfill_marks.remove(id);
        }
        stats.cursor = to_block;
    }
    Ok(stats)
}

/// Runs every configured chain scanner until one task fails.
pub async fn run_from_env(pool: PgPool) -> Result<(), ScannerError> {
    let paths = route_paths_from_env()?;
    let chains = load_route_files(&paths)?;
    let poll_interval = poll_interval_from_env()?;
    let mut tasks = JoinSet::new();
    for routes in chains {
        let provider_id = routes
            .chain
            .rpc_providers
            .first()
            .ok_or_else(|| ScannerError::Configuration("chain has no provider A".to_owned()))?;
        let environment = provider_environment_name(provider_id);
        let rpc_url = std::env::var(&environment).map_err(|_| {
            ScannerError::Configuration(format!("{environment} is required for provider A"))
        })?;
        let reader = EvmChain::new(&rpc_url)?;
        let chain_pool = pool.clone();
        tasks.spawn(async move { run_chain(chain_pool, reader, routes, poll_interval).await });
    }

    match tasks.join_next().await {
        Some(Ok(Err(error))) => Err(error),
        Some(Err(error)) => Err(ScannerError::Task(error.to_string())),
        Some(Ok(Ok(()))) => Err(ScannerError::Task(
            "chain scanner exited unexpectedly".to_owned(),
        )),
        None => Err(ScannerError::Configuration(
            "no chain scanners were configured".to_owned(),
        )),
    }
}

async fn run_chain(
    pool: PgPool,
    reader: EvmChain,
    routes: ChainRoutes,
    poll_interval: Duration,
) -> Result<(), ScannerError> {
    loop {
        let stats = scan_once(&pool, &reader, &routes).await?;
        tracing::info!(
            chain_id = routes.chain.chain_id,
            cursor = stats.cursor,
            inserted = stats.inserted,
            backfilled_addresses = stats.backfilled_addresses,
            "finalized chain scan committed"
        );
        tokio::time::sleep(poll_interval).await;
    }
}

fn route_paths_from_env() -> Result<Vec<PathBuf>, ScannerError> {
    let raw = std::env::var_os("TOPUP_ROUTE_FILES")
        .ok_or_else(|| ScannerError::Configuration("TOPUP_ROUTE_FILES is required".to_owned()))?;
    let paths = std::env::split_paths(&raw).collect::<Vec<_>>();
    if paths.is_empty() || paths.iter().any(|path| path.as_os_str().is_empty()) {
        return Err(ScannerError::Configuration(
            "TOPUP_ROUTE_FILES contains an empty path".to_owned(),
        ));
    }
    Ok(paths)
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

fn provider_environment_name(provider_id: &str) -> String {
    let normalized = provider_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    format!("TOPUP_RPC_{normalized}_URL")
}

fn address_index(addresses: &[ScanAddress]) -> BTreeMap<Address, ScanAddress> {
    addresses
        .iter()
        .cloned()
        .map(|address| (address.address, address))
        .collect()
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
    use std::sync::Mutex;

    use alloy_primitives::{B256, U256};
    use chrono::DateTime;
    use topup_core::money::AtomicAmount;
    use uuid::Uuid;

    use super::*;

    #[derive(Default)]
    struct MockChainReader {
        requests: Mutex<Vec<(usize, u64, u64)>>,
    }

    impl ChainReader for MockChainReader {
        async fn finalized_head(&self) -> Result<u64, ChainError> {
            Ok(0)
        }

        async fn transfer_logs_to(
            &self,
            addresses: &[Address],
            from_block: u64,
            to_block: u64,
        ) -> Result<Vec<TransferLog>, ChainError> {
            self.requests.lock().expect("request lock").push((
                addresses.len(),
                from_block,
                to_block,
            ));
            Ok(Vec::new())
        }
    }

    #[test]
    fn scanner_windows_are_inclusive_and_bounded() {
        assert_eq!(
            scan_windows(1, 4_001).expect("valid range"),
            vec![(1, 2_000), (2_001, 4_000), (4_001, 4_001)]
        );
    }

    #[tokio::test]
    async fn mocked_chain_reader_observes_scanner_windows() {
        let reader = MockChainReader::default();
        let addresses = vec![Address::ZERO; 1_001];
        for (from_block, to_block) in scan_windows(1, 4_001).expect("valid range") {
            reader
                .transfer_logs_to(&addresses, from_block, to_block)
                .await
                .expect("mocked request");
        }
        assert_eq!(
            *reader.requests.lock().expect("request lock"),
            vec![
                (1_001, 1, 2_000),
                (1_001, 2_001, 4_000),
                (1_001, 4_001, 4_001),
            ]
        );
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
        let yaml = include_str!("../../tests/fixtures/phala-cloud-pha.yaml").replace(
            "0x6c5bA91642F10282b576d91922Ae6448C9d52f4E",
            &format!("{token:#x}"),
        );
        let route: RouteFile = serde_saphyr::from_str(&yaml).expect("route fixture");
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
}
