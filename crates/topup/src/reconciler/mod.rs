//! Periodic custody and settlement reconciliation.
//!
//! Every §13 check runs on every round, independently of the others. Per-deposit failures are
//! recorded as findings; a check that cannot complete is reported in
//! [`ReconciliationReport::failed_checks`] and withholds the round heartbeat.

mod chain;
mod store;
mod types;

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use sqlx::{PgPool, Row};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::MAX_ADDRESSES_PER_REQUEST;
use topup_adapters::settlement::http::{SettlementAnswer, SettlementClient, SettlementClientError};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::deposit::{DepositState, StepOutcome, TransitionKind, next};
use topup_core::money::{PRICE_SCALE, ScaledPrice, credit};
use topup_core::route::{RouteFile, product_destination};
use uuid::Uuid;

use crate::db::{self, ApplyTransitionError, ApplyTransitionResult, ScanAddress};
use crate::locks::{self, RateLockError};
use crate::pump::StepResult;
use crate::scanner::{
    ChainRoutes, MAX_SCAN_WINDOW, ScannerError, configure_routes, resolve_logs_for_reconciliation,
};
use crate::steps::settle::{SettleStepError, adopt_answer, validate_answer_identity};

use store::{CustodyCursor, state_code};

pub use chain::{ReconciliationChain, RpcReconciliationChain};
pub use store::{
    LeaseOwnerLock, blocked_addresses, chain_is_blocked, frozen_chains, hold_lease_owner_lock,
};
pub use types::{CheckName, Finding, ReconciliationReport};

/// Maximum `eth_getLogs` windows one incremental scan advances per chain and round.
const MAX_WINDOWS_PER_ROUND: usize = 64;

const LOOP_NAME: &str = "reconciler";
const LOOP_INSTANCE: &str = "0";

/// Order in which a round runs its checks; derivation runs first so a freeze lands early.
const REGULAR_CHECKS: [CheckName; 7] = [
    CheckName::AddressDerivation,
    CheckName::MissingDeposit,
    CheckName::SentSettlement,
    CheckName::CreditRecomputation,
    CheckName::MissingFlushLink,
    CheckName::CustodyBalance,
    CheckName::LockExposure,
];

/// Reconciliation failure which prevents one check or one subject from completing.
#[derive(Debug)]
pub enum ReconciliationError {
    /// Runtime configuration is invalid or incomplete.
    Configuration(String),
    /// A chain adapter failed.
    Chain(String),
    /// A product settlement lookup failed.
    Settlement(String),
    /// PostgreSQL failed an operation.
    Database(sqlx::Error),
    /// A finding could not be encoded.
    Encode(serde_json::Error),
    /// Durable data violated an internal invariant.
    Invariant(&'static str),
    /// The lease-owner lock is held in a conflicting mode by another process.
    LeaseOwnerLock(&'static str),
}

impl ReconciliationError {
    /// Returns a stable category safe to persist in findings.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Configuration(_) => "configuration",
            Self::Chain(_) => "chain_unavailable",
            Self::Settlement(_) => "settlement_lookup_failed",
            Self::Database(_) => "database",
            Self::Encode(_) => "encode",
            Self::Invariant(message) => message,
            Self::LeaseOwnerLock(_) => "lease_owner_lock_held",
        }
    }
}

impl Display for ReconciliationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) | Self::Chain(message) | Self::Settlement(message) => {
                formatter.write_str(message)
            }
            Self::Database(error) => Display::fmt(error, formatter),
            Self::Encode(error) => Display::fmt(error, formatter),
            Self::Invariant(message) | Self::LeaseOwnerLock(message) => {
                formatter.write_str(message)
            }
        }
    }
}

impl Error for ReconciliationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Configuration(_)
            | Self::Chain(_)
            | Self::Settlement(_)
            | Self::Invariant(_)
            | Self::LeaseOwnerLock(_) => None,
        }
    }
}

impl From<sqlx::Error> for ReconciliationError {
    fn from(error: sqlx::Error) -> Self {
        Self::Database(error)
    }
}

impl From<serde_json::Error> for ReconciliationError {
    fn from(error: serde_json::Error) -> Self {
        Self::Encode(error)
    }
}

impl From<topup_adapters::chain::evm::ChainError> for ReconciliationError {
    fn from(error: topup_adapters::chain::evm::ChainError) -> Self {
        Self::Chain(error.to_string())
    }
}

impl From<crate::flusher::ChainError> for ReconciliationError {
    fn from(error: crate::flusher::ChainError) -> Self {
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

/// Product GET boundary used by normal and post-restore reconciliation.
#[async_trait]
pub trait SettlementLookup: Send + Sync {
    /// Fetches one product answer from the configured settlement endpoint.
    async fn get_by_key(
        &self,
        settlement_url: &str,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, ReconciliationError>;
}

struct SignedSettlementLookup {
    signer: SignerHandle,
    timeout: Duration,
}

#[async_trait]
impl SettlementLookup for SignedSettlementLookup {
    async fn get_by_key(
        &self,
        settlement_url: &str,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, ReconciliationError> {
        let client = SettlementClient::new(settlement_url, self.signer.clone(), self.timeout)
            .map_err(map_settlement)?;
        topup_adapters::settlement::http::SettlementApi::get_by_key(&client, key)
            .await
            .map_err(map_settlement)
    }
}

fn map_settlement(error: SettlementClientError) -> ReconciliationError {
    ReconciliationError::Settlement(error.to_string())
}

/// Finalized heads read once per chain and shared by every check in a round.
type FinalizedHeads = BTreeMap<u64, u64>;

/// Product decision carried by a terminal settlement answer.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ProductDecision {
    Credited,
    Rejected,
}

impl ProductDecision {
    const fn code(self) -> &'static str {
        match self {
            Self::Credited => "credited",
            Self::Rejected => "rejected",
        }
    }

    const fn agrees_with(self, state: DepositState) -> bool {
        match self {
            Self::Credited => matches!(state, DepositState::Credited | DepositState::Swept),
            Self::Rejected => matches!(state, DepositState::Rejected),
        }
    }

    const fn target(self, deposit: &db::Deposit) -> DepositState {
        match self {
            Self::Credited if deposit.flush_id.is_some() => DepositState::Swept,
            Self::Credited => DepositState::Credited,
            Self::Rejected => DepositState::Rejected,
        }
    }
}

/// Runs every §13 check against configured routes and dependencies.
pub struct Reconciler {
    pool: PgPool,
    routes: Vec<RouteFile>,
    scanner_routes: BTreeMap<u64, ChainRoutes>,
    chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
    settlement: Arc<dyn SettlementLookup>,
}

impl Reconciler {
    /// Builds production reconciliation dependencies from attested route files.
    pub fn from_routes(
        pool: PgPool,
        routes: Vec<RouteFile>,
        signer: SignerHandle,
    ) -> Result<Self, ReconciliationError> {
        let mut chains = BTreeMap::<u64, Arc<dyn ReconciliationChain>>::new();
        for route in &routes {
            if chains.contains_key(&route.chain.chain_id) {
                continue;
            }
            let provider = route.chain.rpc_providers.first().ok_or_else(|| {
                ReconciliationError::Configuration(format!(
                    "route `{}` has no reconciliation RPC provider",
                    route.route
                ))
            })?;
            let url =
                crate::rpc_provider::configured_provider_url(provider).map_err(|environment| {
                    ReconciliationError::Configuration(format!(
                        "{environment} is required for reconciler route `{}`",
                        route.route
                    ))
                })?;
            let chain = RpcReconciliationChain::connect(
                &url,
                crate::rpc_provider::RPC_TIMEOUT,
                crate::rpc_provider::BALANCE_BATCH_SIZE,
            )?
            .with_provider(&crate::rpc_provider::provider_label(provider, 0));
            chains.insert(route.chain.chain_id, Arc::new(chain));
        }
        let settlement = Arc::new(SignedSettlementLookup {
            signer,
            timeout: Duration::from_secs(30),
        });
        Self::with_dependencies(pool, routes, chains, settlement)
    }

    /// Builds a reconciler with explicit dependencies for integration tests.
    pub fn with_dependencies(
        pool: PgPool,
        routes: Vec<RouteFile>,
        chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
        settlement: Arc<dyn SettlementLookup>,
    ) -> Result<Self, ReconciliationError> {
        let scanner_routes = configure_routes(&routes)?
            .into_iter()
            .map(|route| (route.chain.chain_id, route))
            .collect();
        Ok(Self {
            pool,
            routes,
            scanner_routes,
            chains,
            settlement,
        })
    }

    /// Runs all regular reconciliation checks once.
    ///
    /// Check failures are reported in [`ReconciliationReport::failed_checks`]; the other checks
    /// still run. The `Result` is kept for callers of the library entry point.
    pub async fn run_once(&self) -> Result<ReconciliationReport, ReconciliationError> {
        Ok(self.run_checks(false).await)
    }

    /// Runs the restore gate: every regular check plus authoritative product GETs.
    ///
    /// The gate preempts restored deposit leases, so it refuses to run while any process holds
    /// the [`LeaseOwnerLock`]. [`ReconciliationReport::incomplete`] is driven only by the
    /// product GETs.
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
    /// Safe repairs still apply: `missing_deposit`, `missing_flush_link`, `sent_settlement`,
    /// and `lock_exposure` write the ledger exactly as a full round does.
    pub async fn check(&self, check: CheckName) -> Result<Vec<Finding>, ReconciliationError> {
        let mut findings = Vec::new();
        self.run_check(check, &mut FinalizedHeads::new(), &mut findings)
            .await?;
        Ok(findings)
    }

    async fn run_checks(&self, post_restore: bool) -> ReconciliationReport {
        let mut heads = FinalizedHeads::new();
        let mut report = ReconciliationReport::default();
        let checks = REGULAR_CHECKS
            .into_iter()
            // The post-restore GETs cover every sent settlement at or beyond `cleared`.
            .filter(|check| !(post_restore && *check == CheckName::SentSettlement))
            .chain(post_restore.then_some(CheckName::PostRestoreSettlement));
        for check in checks {
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
            }
            report.findings.extend(findings);
        }
        report.incomplete = report.findings.iter().any(|finding| finding.incomplete)
            || (post_restore
                && report
                    .failed_checks
                    .contains(&CheckName::PostRestoreSettlement));
        if report.succeeded() {
            crate::observability::progress(LOOP_NAME, LOOP_INSTANCE);
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
            CheckName::SentSettlement => self.sent_settlements(findings).await,
            CheckName::CreditRecomputation => self.credit_recomputation(findings).await,
            CheckName::MissingFlushLink => self.missing_flush_links(findings).await,
            CheckName::CustodyBalance => self.custody_balances(heads, findings).await,
            CheckName::PostRestoreSettlement => self.post_restore_settlements(findings).await,
            CheckName::LockExposure => self.lock_exposure(findings).await,
        }
    }

    /// Runs periodic reconciliation until cancellation.
    pub async fn run_loop(&self, every: Duration, cancellation: CancellationToken) {
        crate::observability::register_loop(LOOP_NAME, LOOP_INSTANCE);
        let mut ticks = interval(every);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticks.tick() => {
                    crate::observability::heartbeat(LOOP_NAME, LOOP_INSTANCE);
                    // A round may legitimately run past the heartbeat threshold; it is overdue
                    // only once it overruns the interval that schedules the next round.
                    crate::observability::execution_deadline(LOOP_NAME, LOOP_INSTANCE, every);
                    tokio::select! {
                        () = cancellation.cancelled() => return,
                        _report = self.run_checks(false) => {}
                    }
                    crate::observability::waiting(LOOP_NAME, LOOP_INSTANCE, every);
                }
            }
        }
    }

    /// Verifies every stored salt with the on-chain factory and freezes mismatching chains.
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
        let addresses = db::list_chain_addresses(&self.pool, chain_id).await?;
        let salts = addresses
            .iter()
            .map(|address| address.salt)
            .collect::<Vec<_>>();
        let derived = chain.factory_addresses(factory, &salts).await?;
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
                "factory addressOf(salt) disagrees with stored address",
            )
            .await?;
            findings.push(Finding::new(
                CheckName::AddressDerivation,
                subjects([
                    ("chain_id", chain_id.to_string()),
                    ("address_id", stored.id.to_string()),
                    ("salt", format!("{:#x}", stored.salt)),
                ]),
                json!({"address": format!("{observed:#x}")}),
                json!({"address": format!("{:#x}", stored.address)}),
                false,
                false,
            )?);
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

    /// Adopts product answers for sent settlements under a deposit lease.
    async fn sent_settlements(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT s.deposit_id
            FROM settlements s
            JOIN deposits d ON d.id = s.deposit_id
            WHERE s.status = 'sent' AND d.state = 'cleared'
            ORDER BY s.deposit_id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        for id in ids {
            self.push_settlement_result(id, false, findings).await?;
        }
        Ok(())
    }

    /// GETs every deposit at or beyond cleared before a restored service resumes.
    async fn post_restore_settlements(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        let ids = sqlx::query_scalar::<_, Uuid>(
            r#"
            SELECT id FROM deposits
            WHERE state IN ('cleared', 'credited', 'swept')
               OR (state = 'rejected' AND reason = 'product_refused')
            ORDER BY id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        for id in ids {
            self.push_settlement_result(id, true, findings).await?;
        }
        Ok(())
    }

    async fn push_settlement_result(
        &self,
        deposit_id: Uuid,
        post_restore: bool,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        match self.reconcile_settlement(deposit_id, post_restore).await {
            Ok(Some(finding)) => findings.push(finding),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(%deposit_id, %error, "settlement reconciliation failed for deposit");
                findings.push(Finding::new(
                    settlement_check(post_restore),
                    subjects([("deposit_id", deposit_id.to_string())]),
                    json!({"product_answer": "reconciled"}),
                    json!({"error": error.code()}),
                    false,
                    post_restore,
                )?);
            }
        }
        Ok(())
    }

    async fn reconcile_settlement(
        &self,
        deposit_id: Uuid,
        post_restore: bool,
    ) -> Result<Option<Finding>, ReconciliationError> {
        let lease_token = Uuid::new_v4();
        if !store::claim_deposit(&self.pool, deposit_id, lease_token, post_restore).await? {
            if post_restore {
                return Ok(Some(Finding::new(
                    CheckName::PostRestoreSettlement,
                    subjects([("deposit_id", deposit_id.to_string())]),
                    json!({"deposit_lease": "claimed"}),
                    json!({"deposit_lease": "busy"}),
                    false,
                    true,
                )?));
            }
            tracing::debug!(%deposit_id, "deposit is leased elsewhere; settlement adoption skipped");
            return Ok(None);
        }
        let result = self
            .reconcile_claimed_settlement(deposit_id, lease_token, post_restore)
            .await;
        let released = store::release_lease(&self.pool, deposit_id, lease_token).await;
        let finding = result?;
        released?;
        Ok(finding)
    }

    /// Fetches the product answer from the product's attested route destination.
    async fn settlement_answer(
        &self,
        product: &str,
        key: &str,
    ) -> Result<Option<SettlementAnswer>, ReconciliationError> {
        let destination = product_destination(&self.routes, product)
            .map_err(|error| ReconciliationError::Configuration(error.to_string()))?
            .ok_or_else(|| {
                ReconciliationError::Configuration(format!(
                    "no loaded route names product `{product}`"
                ))
            })?;
        self.settlement
            .get_by_key(&destination.settlement_url, key)
            .await
    }

    async fn reconcile_claimed_settlement(
        &self,
        deposit_id: Uuid,
        lease_token: Uuid,
        post_restore: bool,
    ) -> Result<Option<Finding>, ReconciliationError> {
        let deposit = db::get_deposit(&self.pool, deposit_id).await?.ok_or(
            ReconciliationError::Invariant("settlement deposit is missing"),
        )?;
        let row = sqlx::query(
            r#"
            SELECT a.external_id, a.product_id, p.slug AS product,
                   COALESCE(s.key, 'deposit:' || d.id::text) AS key,
                   s.status AS settlement_status
            FROM deposits d
            JOIN accounts a ON a.id = d.account_id
            JOIN products p ON p.id = a.product_id
            LEFT JOIN settlements s ON s.deposit_id = d.id
            WHERE d.id = $1
            "#,
        )
        .bind(deposit_id)
        .fetch_one(&self.pool)
        .await?;
        let external_id: String = row.try_get("external_id")?;
        let product_id: Uuid = row.try_get("product_id")?;
        let product: String = row.try_get("product")?;
        let key: String = row.try_get("key")?;
        let settlement_status: Option<String> = row.try_get("settlement_status")?;
        let local = state_code(deposit.state);
        // Only the post-restore gate reaches deposits beyond `cleared`.
        let locally_terminal = deposit.state != DepositState::Cleared;
        let check = settlement_check(post_restore);
        let finding = |expected, observed, repair_applied, incomplete| {
            Finding::new(
                check,
                subjects([("deposit_id", deposit_id.to_string()), ("key", key.clone())]),
                expected,
                observed,
                repair_applied,
                incomplete,
            )
        };

        let answer = match self.settlement_answer(&product, &key).await {
            Ok(answer) => answer,
            Err(error) => {
                tracing::warn!(%deposit_id, %error, "product settlement lookup failed");
                return Ok(Some(finding(
                    json!({"product_answer": "available"}),
                    json!({"local_state": local, "error": error.code()}),
                    false,
                    post_restore,
                )?));
            }
        };
        let Some(answer) = answer else {
            if !locally_terminal {
                // The settle step GETs before any resend, so a cleared deposit is complete.
                tracing::debug!(%deposit_id, "product has no settlement record yet");
                return Ok(None);
            }
            return Ok(Some(finding(
                json!({"product_answer": "terminal"}),
                json!({"local_state": local, "product_answer": null}),
                false,
                true,
            )?));
        };
        let answer_kind = settlement_answer_kind(&answer);
        if let Err(error) = validate_answer_identity(&deposit, &external_id, &answer) {
            return Ok(Some(finding(
                json!({"product_answer": "identity_verified"}),
                json!({"product_answer": answer_kind, "error": error.code()}),
                false,
                post_restore,
            )?));
        }
        let decision = match &answer {
            SettlementAnswer::Accepted { .. } => ProductDecision::Credited,
            SettlementAnswer::Rejected { .. } => ProductDecision::Rejected,
            SettlementAnswer::Processing { .. } | SettlementAnswer::Conflict409 => {
                if !locally_terminal {
                    tracing::debug!(%deposit_id, "product is still processing the settlement");
                    return Ok(None);
                }
                return Ok(Some(finding(
                    json!({"product_answer": "terminal"}),
                    json!({"local_state": local, "product_answer": answer_kind}),
                    false,
                    true,
                )?));
            }
            SettlementAnswer::PayloadMismatch422 | SettlementAnswer::Unknown { .. } => {
                return Ok(Some(finding(
                    json!({"product_answer": "terminal"}),
                    json!({"local_state": local, "product_answer": answer_kind}),
                    false,
                    post_restore,
                )?));
            }
        };
        if settlement_status.is_none()
            && let Some(payload) = answer_payload(&answer)
        {
            db::upsert_intent(
                &self.pool,
                &db::SettlementIntent {
                    deposit_id,
                    product_id,
                    key: key.clone(),
                    payload,
                },
            )
            .await?;
        }
        let result =
            match adopt_answer(&self.pool, &deposit, product_id, &external_id, answer).await {
                Ok(result) => result,
                Err(SettleStepError::Database(error)) => return Err(error.into()),
                Err(error) => {
                    return Ok(Some(finding(
                        json!({"product_answer": "adopted"}),
                        json!({"product_answer": answer_kind, "error": error.code()}),
                        false,
                        post_restore,
                    )?));
                }
            };
        let expected = json!({"local_state": decision.code()});
        let observed = json!({"local_state": local, "product_answer": answer_kind});
        if decision.agrees_with(deposit.state) {
            let settlement_adopted =
                !matches!(settlement_status.as_deref(), Some("accepted" | "rejected"));
            return settlement_adopted
                .then(|| finding(expected, observed, true, false))
                .transpose()
                .map_err(Into::into);
        }
        let applied = if deposit.state == DepositState::Cleared {
            self.apply_answer_transition(&deposit, lease_token, &result)
                .await?
        } else if post_restore {
            store::apply_product_answer(
                &self.pool,
                &deposit,
                lease_token,
                decision.target(&deposit),
                &result.evidence,
                &result.events,
            )
            .await?
        } else {
            false
        };
        Ok(Some(finding(
            expected,
            observed,
            applied,
            post_restore && !applied,
        )?))
    }

    /// Applies an adopted answer to a leased `cleared` deposit through `core::next`.
    async fn apply_answer_transition(
        &self,
        deposit: &db::Deposit,
        lease_token: Uuid,
        result: &StepResult,
    ) -> Result<bool, ReconciliationError> {
        let transition = next(deposit.state, &result.outcome).map_err(|_| {
            ReconciliationError::Invariant("adopted product answer is not a valid transition")
        })?;
        let update = db::TransitionUpdate {
            transition,
            rejection_reason: match result.outcome {
                StepOutcome::Reject(reason) => Some(reason),
                _ => None,
            },
            attempt: match transition.kind {
                TransitionKind::Advanced => 0,
                TransitionKind::Rejected | TransitionKind::Retry | TransitionKind::Wait => {
                    deposit.attempt
                }
            },
            next_attempt_at: Utc::now(),
        };
        let mut transaction = self.pool.begin().await?;
        let applied = db::apply_transition(
            &mut transaction,
            deposit.id,
            deposit.state,
            lease_token,
            update,
            db::TransitionWrites {
                evidence: &result.evidence,
                effects: &result.effects,
                outbox_events: &result.events,
            },
        )
        .await?;
        if applied == ApplyTransitionResult::Applied {
            transaction.commit().await?;
            Ok(true)
        } else {
            transaction.rollback().await?;
            Ok(false)
        }
    }

    /// Recomputes stored credit and blocks flushing for every mismatching address.
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
                "SELECT credit_minor::text FROM rate_locks WHERE address_id = $1",
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
        store::block_address(
            &self.pool,
            deposit.chain_id,
            deposit.address_id,
            CheckName::CreditRecomputation.code(),
            "stored credit disagrees with deterministic recomputation",
        )
        .await?;
        Ok(Some(Finding::new(
            CheckName::CreditRecomputation,
            deposit_subjects(),
            json!({"credit_minor": expected.to_string()}),
            json!({"credit_minor": stored.value().to_string()}),
            false,
            false,
        )?))
    }

    /// Replays confirmed flush linkage with the same atomic rule the flusher uses.
    async fn missing_flush_links(
        &self,
        findings: &mut Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        for flush_id in store::linkable_flushes(&self.pool).await? {
            for deposit_id in db::link_confirmed_flush(&self.pool, flush_id).await? {
                findings.push(Finding::new(
                    CheckName::MissingFlushLink,
                    subjects([
                        ("deposit_id", deposit_id.to_string()),
                        ("flush_id", flush_id.to_string()),
                    ]),
                    json!({"flush_id": flush_id}),
                    json!({"flush_id": null}),
                    true,
                    false,
                )?);
            }
        }
        Ok(())
    }

    /// Recomputes drifted rate-lock exposure counters from their open reserved locks.
    ///
    /// The repair is safe in both directions: it writes the value every writer maintains
    /// incrementally, computed under the counter's row lock (see [`locks::repair_exposure`]).
    async fn lock_exposure(&self, findings: &mut Vec<Finding>) -> Result<(), ReconciliationError> {
        let repairs = locks::repair_exposure(&self.pool)
            .await
            .map_err(|error| match error {
                RateLockError::Database(error) => ReconciliationError::Database(error),
                _ => ReconciliationError::Invariant("lock exposure is outside u64"),
            })?;
        for repair in repairs {
            findings.push(Finding::new(
                CheckName::LockExposure,
                // The repair id keeps a recurring identical drift from deduplicating into one row.
                subjects([
                    ("scope_key", repair.scope_key),
                    ("repair_id", repair.id.to_string()),
                ]),
                json!({"open_minor": repair.after_minor.to_string()}),
                json!({"open_minor": repair.before_minor.to_string()}),
                true,
                false,
            )?);
        }
        Ok(())
    }

    /// Compares finalized custody state with the durable ledger at the same block.
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
        let addresses = db::list_scan_addresses(&self.pool, chain_id).await?;

        // Flushes not yet confirmed may already have moved funds at the finalized block.
        let in_flight = store::in_flight_flush_addresses(&self.pool, chain_id, token).await?;
        let checked = addresses
            .iter()
            .filter(|address| !in_flight.contains(&address.id))
            .collect::<Vec<_>>();
        let physical = checked
            .iter()
            .map(|address| address.address)
            .collect::<Vec<_>>();
        let balances = if physical.is_empty() {
            Vec::new()
        } else {
            chain.token_balances(token, &physical, finalized).await?
        };
        if balances.len() != checked.len() {
            return Err(ReconciliationError::Invariant(
                "balance response length did not match address count",
            ));
        }
        let totals = store::address_totals(&self.pool, chain_id, token, finalized)
            .await?
            .into_iter()
            .map(|(id, deposits, flushed)| (id, (deposits, flushed)))
            .collect::<BTreeMap<_, _>>();
        for (address, observed) in checked.iter().zip(balances) {
            let (deposits, flushed) =
                totals
                    .get(&address.id)
                    .ok_or(ReconciliationError::Invariant(
                        "address accounting total is missing",
                    ))?;
            if deposits.checked_sub(*flushed) == Some(observed) {
                continue;
            }
            findings.push(Finding::new(
                CheckName::CustodyBalance,
                subjects([
                    ("chain_id", chain_id.to_string()),
                    ("address_id", address.id.to_string()),
                    ("token", format!("{token:#x}")),
                ]),
                json!({
                    "deposits_atomic": deposits.to_string(),
                    "flushed_atomic": flushed.to_string(),
                }),
                json!({"balance_atomic": observed.to_string()}),
                false,
                false,
            )?);
        }

        // Only transfers from our forwarders count as inflow: the treasury also receives finance
        // top-ups and other funds that no `Flushed` event accounts for, so comparing its total
        // inflow would alert on every such transfer.
        let factory = route.chain.contracts.forwarder_factory;
        let treasury = route.chain.contracts.treasury;
        let cursor = store::custody_cursor(&self.pool, chain_id, factory, token).await?;
        let Some(start) = cursor
            .map(|cursor| cursor.next_block)
            .or_else(|| first_created_block(&addresses))
        else {
            return Ok(());
        };
        let forwarders = addresses
            .iter()
            .map(|address| address.address)
            .collect::<BTreeSet<_>>();
        let mut totals = cursor.unwrap_or(CustodyCursor {
            next_block: start,
            flushed_event_total: U256::ZERO,
            treasury_inflow_total: U256::ZERO,
        });
        for (from_block, to_block) in bounded_windows(start, finalized)? {
            let flushed = chain
                .flushed_total(factory, token, from_block, to_block)
                .await?;
            let inflow =
                treasury_inflow(&chain, treasury, token, &forwarders, from_block, to_block).await?;
            totals = CustodyCursor {
                next_block: next_block(to_block)?,
                flushed_event_total: checked_add(totals.flushed_event_total, flushed)?,
                treasury_inflow_total: checked_add(totals.treasury_inflow_total, inflow)?,
            };
        }
        if cursor != Some(totals)
            && !store::advance_custody_cursor(&self.pool, chain_id, factory, token, cursor, totals)
                .await?
        {
            tracing::debug!(chain_id, "custody cursor advanced concurrently");
            return Ok(());
        }
        if totals.treasury_inflow_total != totals.flushed_event_total {
            findings.push(Finding::new(
                CheckName::CustodyBalance,
                subjects([
                    ("chain_id", chain_id.to_string()),
                    ("treasury", format!("{treasury:#x}")),
                    ("token", format!("{token:#x}")),
                ]),
                json!({"flushed_event_total": totals.flushed_event_total.to_string()}),
                json!({"treasury_inflow_total": totals.treasury_inflow_total.to_string()}),
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
            .iter()
            .map(|route| ((route.route.clone(), route.version), route))
            .collect()
    }

    fn latest_asset_routes(&self) -> Vec<&RouteFile> {
        let mut latest = BTreeMap::<(u64, Address), &RouteFile>::new();
        for route in &self.routes {
            latest
                .entry((route.chain.chain_id, route.asset.contract))
                .and_modify(|current| {
                    if route.version > current.version {
                        *current = route;
                    }
                })
                .or_insert(route);
        }
        latest.into_values().collect()
    }

    fn chain_factories(&self) -> Result<Vec<(u64, Address)>, ReconciliationError> {
        let mut factories = BTreeMap::new();
        for route in &self.routes {
            match factories.insert(
                route.chain.chain_id,
                route.chain.contracts.forwarder_factory,
            ) {
                Some(existing) if existing != route.chain.contracts.forwarder_factory => {
                    return Err(ReconciliationError::Configuration(format!(
                        "routes disagree on factory for chain {}",
                        route.chain.chain_id
                    )));
                }
                Some(_) | None => {}
            }
        }
        Ok(factories.into_iter().collect())
    }
}

/// Runs the library post-restore gate for callers such as the D3 restore check.
pub async fn post_restore_once(
    reconciler: &Reconciler,
) -> Result<ReconciliationReport, ReconciliationError> {
    reconciler.post_restore_once().await
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
            check = finding.check.code(),
            subjects = ?finding.subjects,
            expected = %finding.expected,
            observed = %finding.observed,
            metric = types::MISMATCH_METRIC,
            "reconciliation mismatch"
        );
    }
}

const fn settlement_check(post_restore: bool) -> CheckName {
    if post_restore {
        CheckName::PostRestoreSettlement
    } else {
        CheckName::SentSettlement
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

fn checked_add(left: U256, right: U256) -> Result<U256, ReconciliationError> {
    left.checked_add(right)
        .ok_or(ReconciliationError::Invariant(
            "custody total overflowed U256",
        ))
}

fn subjects<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

/// Sums finalized token transfers from our forwarders to the treasury in one window.
async fn treasury_inflow(
    chain: &Arc<dyn ReconciliationChain>,
    treasury: Address,
    token: Address,
    forwarders: &BTreeSet<Address>,
    from_block: u64,
    to_block: u64,
) -> Result<U256, ReconciliationError> {
    let logs = chain
        .transfer_logs_to(std::slice::from_ref(&treasury), from_block, to_block)
        .await?;
    logs.iter()
        .filter(|log| log.token == token && log.to == treasury && forwarders.contains(&log.from))
        .try_fold(U256::ZERO, |total, log| {
            checked_add(total, log.amount.value())
        })
}

fn answer_payload(answer: &SettlementAnswer) -> Option<serde_json::Value> {
    match answer {
        SettlementAnswer::Accepted { payload, .. }
        | SettlementAnswer::Processing { payload }
        | SettlementAnswer::Rejected { payload, .. } => Some(payload.clone()),
        SettlementAnswer::Conflict409
        | SettlementAnswer::PayloadMismatch422
        | SettlementAnswer::Unknown { .. } => None,
    }
}

fn settlement_answer_kind(answer: &SettlementAnswer) -> &'static str {
    match answer {
        SettlementAnswer::Accepted { .. } => "accepted",
        SettlementAnswer::Processing { .. } => "processing",
        SettlementAnswer::Conflict409 => "conflict",
        SettlementAnswer::Rejected { .. } => "rejected",
        SettlementAnswer::PayloadMismatch422 => "payload_mismatch",
        SettlementAnswer::Unknown { .. } => "unknown",
    }
}
