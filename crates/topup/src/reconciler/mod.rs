//! Periodic custody and credit reconciliation.
//!
//! Every §13 check runs on every round, independently of the others. Per-deposit failures are
//! recorded as findings; a check that cannot complete is reported in
//! [`ReconciliationReport::failed_checks`] and withholds the round heartbeat.

mod chain;
mod store;
mod types;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::Address;
use serde_json::json;
use sqlx::PgPool;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::{FinalizedReader, MAX_ADDRESSES_PER_REQUEST};
use topup_core::money::{PRICE_SCALE, ScaledPrice, credit};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db::{self, ApplyTransitionError, ScanAddress};
use crate::routes::RouteSet;
use crate::scanner::{
    ChainRoutes, MAX_SCAN_WINDOW, ScannerError, chain_routes, resolve_logs_for_reconciliation,
};

pub use chain::ReconciliationChain;
pub use store::{LeaseOwnerLock, chain_is_blocked, frozen_chains, hold_lease_owner_lock};
pub use types::{CheckName, Finding, ReconciliationReport};

/// Maximum `eth_getLogs` windows one incremental scan advances per chain and round.
const MAX_WINDOWS_PER_ROUND: usize = 64;

/// Order in which a round runs its checks; derivation runs first so a freeze lands early.
const REGULAR_CHECKS: [CheckName; 5] = [
    CheckName::AddressDerivation,
    CheckName::MissingDeposit,
    CheckName::CreditRecomputation,
    CheckName::MissingFlushLink,
    CheckName::CustodyBalance,
];

/// Reconciliation failure which prevents one check or one subject from completing.
#[derive(Debug, thiserror::Error)]
pub enum ReconciliationError {
    /// Runtime configuration is invalid or incomplete.
    #[error("{0}")]
    Configuration(String),
    /// A chain adapter failed.
    #[error("{0}")]
    Chain(String),
    /// PostgreSQL failed an operation.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
    /// A finding could not be encoded.
    #[error("{0}")]
    Encode(#[from] serde_json::Error),
    /// Durable data violated an internal invariant.
    #[error("{0}")]
    Invariant(&'static str),
    /// The lease-owner lock is held in a conflicting mode by another process.
    #[error("{0}")]
    LeaseOwnerLock(&'static str),
}

impl ReconciliationError {
    /// Returns a stable category safe to persist in findings.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Chain(_) => "chain_unavailable",
            Self::Database(_) => "database",
            Self::Encode(_) => "encode",
            Self::Invariant(message) => message,
            Self::LeaseOwnerLock(_) => "lease_owner_lock_held",
        }
    }
}

impl From<topup_adapters::chain::evm::ChainError> for ReconciliationError {
    fn from(error: topup_adapters::chain::evm::ChainError) -> Self {
        Self::Chain(error.to_string())
    }
}

impl From<ScannerError> for ReconciliationError {
    fn from(error: ScannerError) -> Self {
        Self::Chain(error.to_string())
    }
}

impl From<ApplyTransitionError> for ReconciliationError {
    fn from(error: ApplyTransitionError) -> Self {
        match error {
            ApplyTransitionError::Database(error) => Self::Database(error),
            ApplyTransitionError::InvalidInput(message) => Self::Invariant(message),
        }
    }
}

/// Finalized heads read once per chain and shared by every check in a round.
type FinalizedHeads = BTreeMap<u64, u64>;

/// Runs every §13 check against configured routes and dependencies.
pub struct Reconciler {
    pool: PgPool,
    routes: Arc<RouteSet>,
    scanner_routes: BTreeMap<u64, ChainRoutes>,
    chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
}

impl Reconciler {
    /// Builds production reconciliation dependencies on each chain's provider A.
    pub fn from_routes(pool: PgPool, routes: Arc<RouteSet>) -> Result<Self, ReconciliationError> {
        let mut chains = BTreeMap::<u64, Arc<dyn ReconciliationChain>>::new();
        for chain_id in routes.chain_ids() {
            let client = routes.provider(chain_id, 0).map_err(|error| {
                ReconciliationError::Configuration(format!("reconciler chain {chain_id}: {error}"))
            })?;
            chains.insert(chain_id, Arc::new(FinalizedReader::new(Arc::clone(client))));
        }
        Ok(Self::with_dependencies(pool, routes, chains))
    }

    /// Builds a reconciler with explicit dependencies for integration tests.
    #[must_use]
    pub fn with_dependencies(
        pool: PgPool,
        routes: Arc<RouteSet>,
        chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
    ) -> Self {
        let scanner_routes = chain_routes(&routes)
            .into_iter()
            .map(|route| (route.chain.chain_id, route))
            .collect();
        Self {
            pool,
            routes,
            scanner_routes,
            chains,
        }
    }

    /// Runs all regular reconciliation checks once.
    ///
    /// Check failures are reported in [`ReconciliationReport::failed_checks`]; the other checks
    /// still run. The `Result` is kept for callers of the library entry point.
    pub async fn run_once(&self) -> Result<ReconciliationReport, ReconciliationError> {
        Ok(self.run_checks(false).await)
    }

    /// Runs the post-restore round: every regular check, on the restored ledger alone.
    ///
    /// The service is authoritative for its credits, so a restore asks the product nothing. The
    /// round refuses to run while any process holds the [`LeaseOwnerLock`], so no pump works on
    /// the restored ledger meanwhile.
    pub async fn post_restore_once(&self) -> Result<ReconciliationReport, ReconciliationError> {
        let lock = store::exclusive_lease_owner_lock(&self.pool).await?;
        let report = self.run_checks(true).await;
        if let Err(error) = lock.release().await {
            tracing::warn!(%error, "failed to release the post-restore lease-owner lock");
        }
        Ok(report)
    }

    /// Runs one check without persisting its findings.
    ///
    /// Safe repairs and freezes still apply: `missing_deposit` and `missing_flush_link` write the
    /// ledger, and `address_derivation` and `custody_balance` freeze a chain, exactly as a full
    /// round does.
    pub async fn check(&self, check: CheckName) -> Result<Vec<Finding>, ReconciliationError> {
        let mut findings = Vec::new();
        self.run_check(check, &mut FinalizedHeads::new(), &mut findings)
            .await?;
        Ok(findings)
    }

    async fn run_checks(&self, post_restore: bool) -> ReconciliationReport {
        let mut heads = FinalizedHeads::new();
        let mut report = ReconciliationReport::default();
        for check in REGULAR_CHECKS {
            let mut findings = Vec::new();
            let mut result = self.run_check(check, &mut heads, &mut findings).await;
            for finding in &findings {
                match store::persist_finding(&self.pool, finding).await {
                    Ok(inserted) => log_finding(finding, inserted),
                    Err(error) => result = result.and(Err(error)),
                }
            }
            if let Err(error) = result {
                tracing::error!(check = check.code(), %error, "reconciliation check failed");
                report.failed_checks.push(check);
                report.check_errors.push(error.to_string());
            }
            report.findings.extend(findings);
        }
        report.incomplete = report.findings.iter().any(|finding| finding.incomplete);
        if report.succeeded() {
            tracing::info!(
                findings = report.findings.len(),
                post_restore,
                "reconciler heartbeat"
            );
        } else {
            let failed = report
                .failed_checks
                .iter()
                .map(|check| check.code())
                .collect::<Vec<_>>();
            tracing::error!(?failed, post_restore, "reconciliation round incomplete");
        }
        report
    }

    async fn run_check(
        &self,
        check: CheckName,
        heads: &mut FinalizedHeads,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        match check {
            CheckName::AddressDerivation => self.address_derivation(findings).await,
            CheckName::MissingDeposit => self.missing_deposits(heads, findings).await,
            CheckName::CreditRecomputation => self.credit_recomputation(findings).await,
            CheckName::MissingFlushLink => self.missing_flush_links(findings).await,
            CheckName::CustodyBalance => self.custody_balances(heads, findings).await,
        }
    }

    /// Runs periodic reconciliation until cancellation.
    pub async fn run_loop(&self, every: Duration, cancellation: CancellationToken) {
        let monitor = crate::observability::CronMonitor::reconciler(every);
        let mut ticks = interval(every);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticks.tick() => {
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        report = self.run_checks(false) => {
                            monitor.check_in(report.succeeded());
                            crate::observability::record_reconciliation(
                                report
                                    .failed_checks
                                    .iter()
                                    .map(|check| check.code().to_owned())
                                    .zip(report.check_errors)
                                    .collect(),
                            );
                        }
                    }
                }
            }
        }
    }

    /// Verifies every stored `(salt, treasury)` with the on-chain factory and freezes mismatching
    /// chains.
    async fn address_derivation(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let mut failure = None;
        for (chain_id, factory) in self.chain_factories()? {
            if let Err(error) = self
                .address_derivation_for_chain(chain_id, factory, findings)
                .await
            {
                tracing::error!(chain_id, %error, "address derivation check failed for chain");
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    async fn address_derivation_for_chain(
        &self,
        chain_id: u64,
        factory: Address,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let chain = self.chain(chain_id)?;
        let mut by_treasury = BTreeMap::<Address, Vec<db::Address>>::new();
        for address in db::list_chain_addresses(&self.pool, chain_id).await? {
            by_treasury
                .entry(address.treasury)
                .or_default()
                .push(address);
        }
        for (treasury, addresses) in by_treasury {
            let salts = addresses
                .iter()
                .map(|address| address.salt)
                .collect::<Vec<_>>();
            let derived = chain.factory_addresses(factory, treasury, &salts).await?;
            if derived.len() != addresses.len() {
                return Err(ReconciliationError::Invariant(
                    "addressOf response length did not match address count",
                ));
            }
            for (stored, observed) in addresses.iter().zip(derived) {
                if stored.address == observed {
                    continue;
                }
                store::block_chain(
                    &self.pool,
                    chain_id,
                    CheckName::AddressDerivation.code(),
                    "factory addressOf(treasury, salt) disagrees with stored address",
                )
                .await?;
                findings.push(Finding::new(
                    CheckName::AddressDerivation,
                    subjects([
                        ("chain_id", chain_id.to_string()),
                        ("address_id", stored.id.to_string()),
                        ("salt", format!("{:#x}", stored.salt)),
                        ("treasury", format!("{treasury:#x}")),
                    ]),
                    json!({"address": format!("{observed:#x}")}),
                    json!({"address": format!("{:#x}", stored.address)}),
                    false,
                    false,
                )?);
            }
        }
        Ok(())
    }

    /// Repairs finalized transfers missing from the deposit ledger through the scanner path.
    async fn missing_deposits(
        &self,
        heads: &mut FinalizedHeads,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let mut failure = None;
        for (chain_id, routes) in &self.scanner_routes {
            if let Err(error) = self
                .missing_deposits_for_chain(*chain_id, routes, heads, findings)
                .await
            {
                tracing::error!(chain_id, %error, "missing-deposit check failed for chain");
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Scans incrementally from a durable cursor, at most [`MAX_WINDOWS_PER_ROUND`] windows.
    ///
    /// The scan never passes the range the scanner has committed, so a transfer the scanner has
    /// not reached yet is not reported as missing, and a frozen chain's scan stops with its
    /// scanner. The address list is read after the finalized head and the scanner cursor, so an
    /// address issued later can only receive transfers above the scanned range.
    async fn missing_deposits_for_chain(
        &self,
        chain_id: u64,
        routes: &ChainRoutes,
        heads: &mut FinalizedHeads,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let chain = Arc::clone(self.chain(chain_id)?);
        let finalized = self.finalized(heads, chain_id).await?;
        let Some(scanned) = db::get_cursor(&self.pool, chain_id).await? else {
            tracing::warn!(
                chain_id,
                reason = "no_cursor",
                "missing-deposit check skipped: the scanner has not committed a range"
            );
            return Ok(());
        };
        let addresses = db::list_scan_addresses(&self.pool, chain_id).await?;
        let Some(through) = scanner_covered_through(finalized.min(scanned), &addresses) else {
            tracing::warn!(
                chain_id,
                reason = "pending_backfill",
                "missing-deposit check skipped: an address awaits its scanner backfill"
            );
            return Ok(());
        };
        let mut cursor = store::deposit_cursor(&self.pool, chain_id).await?;
        let Some(start) = cursor.or_else(|| first_created_block(&addresses)) else {
            return Ok(());
        };
        let physical = addresses
            .iter()
            .map(|address| address.address)
            .collect::<Vec<_>>();
        for (from_block, to_block) in bounded_windows(start, through)? {
            for batch in physical.chunks(MAX_ADDRESSES_PER_REQUEST) {
                let logs = chain.transfer_logs_to(batch, from_block, to_block).await?;
                for deposit in resolve_logs_for_reconciliation(logs, &addresses, routes)? {
                    let committed = db::commit_scan(
                        &self.pool,
                        chain_id,
                        std::slice::from_ref(&deposit),
                        &[],
                        None,
                        None,
                    )
                    .await?;
                    if committed.inserted == 0 {
                        continue;
                    }
                    findings.push(Finding::new(
                        CheckName::MissingDeposit,
                        subjects([
                            ("chain_id", chain_id.to_string()),
                            ("tx_hash", format!("{:#x}", deposit.tx_hash)),
                            ("log_index", deposit.log_index.to_string()),
                        ]),
                        json!({"deposit_state": "detected"}),
                        json!({"deposit_row": null}),
                        true,
                        false,
                    )?);
                }
            }
            let next_block = next_block(to_block)?;
            if !store::advance_deposit_cursor(&self.pool, chain_id, cursor, next_block).await? {
                tracing::debug!(chain_id, "missing-deposit cursor advanced concurrently");
                return Ok(());
            }
            cursor = Some(next_block);
        }
        Ok(())
    }

    /// Recomputes stored credit and reports every mismatching deposit.
    async fn credit_recomputation(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id FROM deposits
            WHERE credit_minor IS NOT NULL AND price_scaled IS NOT NULL
              AND route IS NOT NULL AND route_version IS NOT NULL
            ORDER BY id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let routes = self.route_index();
        for id in ids {
            match self.recompute_credit(id, &routes).await {
                Ok(Some(finding)) => findings.push(finding),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(deposit_id = %id, %error, "credit recomputation failed");
                    findings.push(Finding::new(
                        CheckName::CreditRecomputation,
                        subjects([("deposit_id", id.to_string())]),
                        json!({"credit_minor": "recomputed"}),
                        json!({"error": error.code()}),
                        false,
                        false,
                    )?);
                }
            }
        }
        Ok(())
    }

    async fn recompute_credit(
        &self,
        id: Uuid,
        routes: &BTreeMap<(String, u64), &RouteFile>,
    ) -> Result<Option<Finding>, ReconciliationError> {
        let deposit = db::get_deposit(&self.pool, id)
            .await?
            .ok_or(ReconciliationError::Invariant("listed deposit disappeared"))?;
        let route_name = deposit
            .route
            .as_ref()
            .ok_or(ReconciliationError::Invariant(
                "valued deposit has no route",
            ))?;
        let route_version = deposit.route_version.ok_or(ReconciliationError::Invariant(
            "valued deposit has no route version",
        ))?;
        let deposit_subjects = || {
            subjects([
                ("deposit_id", deposit.id.to_string()),
                ("address_id", deposit.address_id.to_string()),
                ("chain_id", deposit.chain_id.to_string()),
            ])
        };
        let Some(route) = routes.get(&(route_name.clone(), route_version)) else {
            return Ok(Some(Finding::new(
                CheckName::CreditRecomputation,
                deposit_subjects(),
                json!({"credit_minor": "recomputed"}),
                json!({
                    "error": "route_version_unavailable",
                    "route": route_name,
                    "route_version": route_version,
                }),
                false,
                false,
            )?));
        };
        let stored = deposit.credit_minor.ok_or(ReconciliationError::Invariant(
            "listed deposit has no stored credit",
        ))?;
        let expected = if deposit.price_source.as_deref() == Some("lock") {
            let value: Option<String> = sqlx::query_scalar(
                "SELECT quote.credit_minor::text FROM quotes AS quote \
                 JOIN addresses AS address ON address.quote_id = quote.id WHERE address.id = $1",
            )
            .bind(deposit.address_id)
            .fetch_optional(&self.pool)
            .await?;
            value
                .ok_or(ReconciliationError::Invariant(
                    "lock-priced deposit has no rate lock",
                ))?
                .parse::<u64>()
                .map_err(|_| ReconciliationError::Invariant("rate-lock credit is invalid"))?
        } else {
            let price = ScaledPrice::new(
                deposit.price_scaled.ok_or(ReconciliationError::Invariant(
                    "listed deposit has no stored price",
                ))?,
                PRICE_SCALE,
            )
            .map_err(|_| ReconciliationError::Invariant("stored price is invalid"))?;
            credit(
                deposit.amount_atomic,
                price,
                route.asset.decimals,
                route.destination.unit_decimals,
            )
            .map_err(|_| ReconciliationError::Invariant("stored credit cannot be recomputed"))?
            .value()
        };
        if expected == stored.value() {
            return Ok(None);
        }
        Ok(Some(Finding::new(
            CheckName::CreditRecomputation,
            deposit_subjects(),
            json!({"credit_minor": expected.to_string()}),
            json!({"credit_minor": stored.value().to_string()}),
            false,
            false,
        )?))
    }

    /// Sweeps every final credited deposit that an indexed finalized `Flushed` event after it
    /// covers, with the rule the scanner and the finality watch apply.
    async fn missing_flush_links(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let mut transaction = self.pool.begin().await?;
        let swept = db::mark_swept(&mut transaction, None, &[]).await?;
        transaction.commit().await?;
        for deposit_id in swept {
            findings.push(Finding::new(
                CheckName::MissingFlushLink,
                subjects([("deposit_id", deposit_id.to_string())]),
                json!({"state": "swept"}),
                json!({"state": "credited"}),
                true,
                false,
            )?);
        }
        Ok(())
    }

    /// Checks, per forwarder, that its finalized balance is its deposits minus its sweeps, and
    /// freezes the chain on any mismatch (design §13).
    async fn custody_balances(
        &self,
        heads: &mut FinalizedHeads,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let mut failure = None;
        for route in self.latest_asset_routes() {
            if let Err(error) = self.custody_for_route(route, heads, findings).await {
                tracing::error!(
                    chain_id = route.chain.chain_id,
                    route = %route.route,
                    %error,
                    "custody balance check failed for route"
                );
                failure.get_or_insert(error);
            }
        }
        failure.map_or(Ok(()), Err)
    }

    /// Compares, at one finalized block, every active forwarder's balance of the route's token
    /// with its deposits minus its finalized `Flushed` amounts.
    ///
    /// The block is the lower of the finalized head and the scanner cursor, below which every
    /// transfer and every factory event of a watched address is indexed. Anyone can flush a
    /// forwarder at any time, and only finalized events are indexed, so both sides describe the
    /// same finalized state; a mismatch means the ledger is wrong, and crediting on the chain
    /// stops until an operator lifts the freeze.
    async fn custody_for_route(
        &self,
        route: &RouteFile,
        heads: &mut FinalizedHeads,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let chain_id = route.chain.chain_id;
        let token = route.asset.contract;
        let chain = Arc::clone(self.chain(chain_id)?);
        let finalized = self.finalized(heads, chain_id).await?;
        let Some(scanned) = db::get_cursor(&self.pool, chain_id).await? else {
            return Ok(());
        };
        let block = finalized.min(scanned);
        let ledgers = store::forwarder_ledgers(&self.pool, chain_id, token, block).await?;
        if ledgers.is_empty() {
            return Ok(());
        }
        let physical = ledgers
            .iter()
            .map(|ledger| ledger.address)
            .collect::<Vec<_>>();
        let balances = chain.token_balances(token, &physical, block).await?;
        if balances.len() != ledgers.len() {
            return Err(ReconciliationError::Invariant(
                "balance response length did not match address count",
            ));
        }
        for (ledger, observed) in ledgers.iter().zip(balances) {
            if ledger.deposits.checked_sub(ledger.flushed) == Some(observed) {
                continue;
            }
            store::block_chain(
                &self.pool,
                chain_id,
                CheckName::CustodyBalance.code(),
                "a forwarder balance disagrees with its deposits minus its sweeps",
            )
            .await?;
            findings.push(Finding::new(
                CheckName::CustodyBalance,
                subjects([
                    ("chain_id", chain_id.to_string()),
                    ("address_id", ledger.address_id.to_string()),
                    ("token", format!("{token:#x}")),
                    ("block", block.to_string()),
                ]),
                json!({
                    "deposits_atomic": ledger.deposits.to_string(),
                    "flushed_atomic": ledger.flushed.to_string(),
                }),
                json!({"balance_atomic": observed.to_string()}),
                false,
                false,
            )?);
        }
        Ok(())
    }

    async fn finalized(
        &self,
        heads: &mut FinalizedHeads,
        chain_id: u64,
    ) -> Result<u64, ReconciliationError> {
        if let Some(head) = heads.get(&chain_id) {
            return Ok(*head);
        }
        let head = self.chain(chain_id)?.finalized_head().await?;
        heads.insert(chain_id, head);
        Ok(head)
    }

    fn chain(&self, chain_id: u64) -> Result<&Arc<dyn ReconciliationChain>, ReconciliationError> {
        self.chains.get(&chain_id).ok_or_else(|| {
            ReconciliationError::Configuration(format!(
                "no reconciliation client for chain {chain_id}"
            ))
        })
    }

    fn route_index(&self) -> BTreeMap<(String, u64), &RouteFile> {
        self.routes
            .routes()
            .iter()
            .map(|route| ((route.route.clone(), route.version), route))
            .collect()
    }

    fn latest_asset_routes(&self) -> Vec<&RouteFile> {
        self.routes.current().collect()
    }

    /// Returns each chain's factory; every route of a chain must name the same one.
    fn chain_factories(&self) -> Result<Vec<(u64, Address)>, ReconciliationError> {
        let mut factories = BTreeMap::new();
        for route in self.routes.routes() {
            let factory = route.chain.contracts.forwarder_factory;
            match factories.insert(route.chain.chain_id, factory) {
                Some(existing) if existing != factory => {
                    return Err(ReconciliationError::Configuration(format!(
                        "routes disagree on the factory for chain {}",
                        route.chain.chain_id
                    )));
                }
                Some(_) | None => {}
            }
        }
        Ok(factories.into_iter().collect())
    }
}

fn log_finding(finding: &Finding, inserted: bool) {
    if !inserted {
        tracing::debug!(
            check = finding.check.code(),
            "reconciliation finding already recorded"
        );
    } else if finding.repair_applied {
        tracing::info!(
            check = finding.check.code(),
            subjects = ?finding.subjects,
            "reconciliation repair applied"
        );
    } else {
        tracing::warn!(
            tags.alert = "TopupReconciliationMismatch",
            tags.check = finding.check.code(),
            check = finding.check.code(),
            subjects = ?finding.subjects,
            expected = %finding.expected,
            observed = %finding.observed,
            "reconciliation mismatch"
        );
    }
}

fn first_created_block(addresses: &[ScanAddress]) -> Option<u64> {
    addresses.iter().map(|address| address.created_block).min()
}

/// Returns the last block the scanner has covered for every address, if any.
///
/// The scanner cursor covers only backfilled addresses; one still awaiting its backfill is
/// covered only below its creation block.
fn scanner_covered_through(scanned: u64, addresses: &[ScanAddress]) -> Option<u64> {
    addresses
        .iter()
        .filter(|address| !address.backfilled)
        .try_fold(scanned, |through, address| {
            Some(through.min(address.created_block.checked_sub(1)?))
        })
}

/// Splits `[from, to]` into scan windows, capped at [`MAX_WINDOWS_PER_ROUND`].
fn bounded_windows(from: u64, to: u64) -> Result<Vec<(u64, u64)>, ReconciliationError> {
    let mut windows = Vec::new();
    let mut start = from;
    while start <= to && windows.len() < MAX_WINDOWS_PER_ROUND {
        let end = start
            .saturating_add(MAX_SCAN_WINDOW.saturating_sub(1))
            .min(to);
        windows.push((start, end));
        if end == to {
            break;
        }
        start = next_block(end)?;
    }
    Ok(windows)
}

fn next_block(block: u64) -> Result<u64, ReconciliationError> {
    block.checked_add(1).ok_or(ReconciliationError::Invariant(
        "reconciliation block range overflowed",
    ))
}

fn subjects<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}
