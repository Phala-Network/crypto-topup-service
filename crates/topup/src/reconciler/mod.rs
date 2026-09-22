//! Periodic custody and settlement reconciliation.

mod chain;
mod store;
mod types;

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::{PgPool, Row};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_adapters::chain::evm::MAX_ADDRESSES_PER_REQUEST;
use topup_adapters::settlement::http::{
    SettlementAnswer, SettlementApi, SettlementClient, SettlementClientError,
};
use topup_adapters::signer::actor::SignerHandle;
use topup_core::deposit::{DepositState, RejectReason, StepOutcome};
use topup_core::money::{PRICE_SCALE, ScaledPrice, credit};
use topup_core::route::RouteFile;
use uuid::Uuid;

use crate::db;
use crate::pump::StepResult;
use crate::scanner::{
    ChainRoutes, MAX_SCAN_WINDOW, ScannerError, configure_routes, resolve_logs_for_reconciliation,
};
use crate::steps::settle::{SettleStepError, adopt_answer, validate_answer_identity};

pub use chain::{ReconciliationChain, RpcReconciliationChain};
pub use store::{blocked_addresses, chain_is_blocked};
pub use types::{CheckName, Finding, ReconciliationMetrics, ReconciliationReport};

/// Reconciliation failure which prevents the current pass from completing.
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
}

impl Display for ReconciliationError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(message) | Self::Chain(message) | Self::Settlement(message) => {
                formatter.write_str(message)
            }
            Self::Database(error) => Display::fmt(error, formatter),
            Self::Encode(error) => Display::fmt(error, formatter),
            Self::Invariant(message) => formatter.write_str(message),
        }
    }
}

impl Error for ReconciliationError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Database(error) => Some(error),
            Self::Encode(error) => Some(error),
            Self::Configuration(_) | Self::Chain(_) | Self::Settlement(_) | Self::Invariant(_) => {
                None
            }
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
        client.get_by_key(key).await.map_err(map_settlement)
    }
}

fn map_settlement(error: SettlementClientError) -> ReconciliationError {
    ReconciliationError::Settlement(error.to_string())
}

/// Runs every §13 check against configured routes and dependencies.
pub struct Reconciler {
    pool: PgPool,
    routes: Vec<RouteFile>,
    scanner_routes: BTreeMap<u64, ChainRoutes>,
    chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
    settlement: Arc<dyn SettlementLookup>,
    metrics: Arc<ReconciliationMetrics>,
}

impl Reconciler {
    /// Builds production reconciliation dependencies from attested route files.
    pub fn from_routes(
        pool: PgPool,
        routes: Vec<RouteFile>,
        signer: SignerHandle,
        metrics: Arc<ReconciliationMetrics>,
    ) -> Result<Self, ReconciliationError> {
        let scanner_routes = configure_routes(&routes)?
            .into_iter()
            .map(|route| (route.chain.chain_id, route))
            .collect::<BTreeMap<_, _>>();
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
            let timeout = Duration::from_millis(route.chain.flush.rpc_timeout_ms);
            let batch = usize::try_from(route.chain.flush.balance_batch_size).map_err(|_| {
                ReconciliationError::Configuration(
                    "reconciliation balance batch size exceeds usize".to_owned(),
                )
            })?;
            let chain = RpcReconciliationChain::connect(&url, timeout, batch)?;
            chains.insert(route.chain.chain_id, Arc::new(chain));
        }
        Ok(Self {
            pool,
            routes,
            scanner_routes,
            chains,
            settlement: Arc::new(SignedSettlementLookup {
                signer,
                timeout: Duration::from_secs(30),
            }),
            metrics,
        })
    }

    /// Builds a reconciler with explicit dependencies for integration tests.
    pub fn with_dependencies(
        pool: PgPool,
        routes: Vec<RouteFile>,
        chains: BTreeMap<u64, Arc<dyn ReconciliationChain>>,
        settlement: Arc<dyn SettlementLookup>,
        metrics: Arc<ReconciliationMetrics>,
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
            metrics,
        })
    }

    /// Runs all regular reconciliation checks once.
    pub async fn run_once(&self) -> Result<ReconciliationReport, ReconciliationError> {
        self.run_checks(false).await
    }

    /// Runs the restore gate, including all regular checks and authoritative product GETs.
    pub async fn post_restore_once(&self) -> Result<ReconciliationReport, ReconciliationError> {
        self.run_checks(true).await
    }

    async fn run_checks(
        &self,
        post_restore: bool,
    ) -> Result<ReconciliationReport, ReconciliationError> {
        let mut findings = Vec::new();
        self.record_findings(&mut findings, self.check_missing_deposits().await?)
            .await?;
        self.record_findings(&mut findings, self.check_sent_settlements().await?)
            .await?;
        self.record_findings(&mut findings, self.check_credit_recomputation().await?)
            .await?;
        self.record_findings(&mut findings, self.check_missing_flush_links().await?)
            .await?;
        self.record_findings(&mut findings, self.check_custody_balances().await?)
            .await?;
        self.record_findings(&mut findings, self.check_address_derivation().await?)
            .await?;
        if post_restore {
            self.record_findings(&mut findings, self.check_post_restore_settlements().await?)
                .await?;
        }
        self.metrics.heartbeat();
        tracing::info!(
            findings = findings.len(),
            post_restore,
            "reconciler heartbeat"
        );
        Ok(ReconciliationReport {
            incomplete: findings
                .iter()
                .any(|finding| finding.incomplete || (post_restore && !finding.repair_applied)),
            findings,
        })
    }

    async fn record_findings(
        &self,
        accumulated: &mut Vec<Finding>,
        findings: Vec<Finding>,
    ) -> Result<(), ReconciliationError> {
        for finding in &findings {
            store::persist_finding(&self.pool, finding, &self.metrics).await?;
            if !finding.repair_applied {
                tracing::warn!(
                    check = finding.check.code(),
                    subjects = ?finding.subjects,
                    expected = %finding.expected,
                    observed = %finding.observed,
                    metric = ReconciliationMetrics::MISMATCH_METRIC,
                    "reconciliation mismatch"
                );
            }
        }
        accumulated.extend(findings);
        Ok(())
    }

    /// Runs periodic reconciliation until cancellation.
    pub async fn run_loop(&self, every: Duration, cancellation: CancellationToken) {
        let mut ticks = interval(every);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticks.tick() => {
                    let result = tokio::select! {
                        () = cancellation.cancelled() => return,
                        result = self.run_once() => result,
                    };
                    if let Err(error) = result {
                        self.metrics.heartbeat();
                        tracing::error!(%error, "reconciliation pass failed");
                        tracing::info!("reconciler heartbeat");
                    }
                }
            }
        }
    }

    /// Repairs finalized transfers missing from the deposit ledger through the scanner path.
    pub async fn check_missing_deposits(&self) -> Result<Vec<Finding>, ReconciliationError> {
        let mut findings = Vec::new();
        for (chain_id, routes) in &self.scanner_routes {
            let chain = self.chain(*chain_id)?;
            let finalized = chain.finalized_head().await?;
            let addresses = db::list_scan_addresses(&self.pool, *chain_id).await?;
            let Some(first_block) = addresses.iter().map(|address| address.created_block).min()
            else {
                continue;
            };
            if first_block > finalized {
                continue;
            }
            let physical = addresses
                .iter()
                .map(|address| address.address)
                .collect::<Vec<_>>();
            for (from_block, to_block) in block_windows(first_block, finalized)? {
                for batch in physical.chunks(MAX_ADDRESSES_PER_REQUEST) {
                    let logs = chain.transfer_logs_to(batch, from_block, to_block).await?;
                    let deposits = resolve_logs_for_reconciliation(logs, &addresses, routes)?;
                    for deposit in deposits {
                        let committed = db::commit_scan(
                            &self.pool,
                            *chain_id,
                            std::slice::from_ref(&deposit),
                            &[],
                            None,
                        )
                        .await?;
                        if committed.inserted == 0 {
                            continue;
                        }
                        let subjects = subjects([
                            ("chain_id", chain_id.to_string()),
                            ("tx_hash", format!("{:#x}", deposit.tx_hash)),
                            ("log_index", deposit.log_index.to_string()),
                        ]);
                        findings.push(Finding::new(
                            CheckName::MissingDeposit,
                            subjects,
                            json!({"deposit_state": "detected"}),
                            json!({"deposit_row": null}),
                            true,
                            false,
                        )?);
                    }
                }
            }
        }
        Ok(findings)
    }

    /// GETs every sent settlement regardless of age and adopts known answers.
    pub async fn check_sent_settlements(&self) -> Result<Vec<Finding>, ReconciliationError> {
        let ids = sqlx::query_scalar::<_, Uuid>(
            "SELECT deposit_id FROM settlements WHERE status = 'sent' ORDER BY deposit_id",
        )
        .fetch_all(&self.pool)
        .await?;
        let mut findings = Vec::new();
        for id in ids {
            if let Some(finding) = self.reconcile_product_answer(id, false).await? {
                findings.push(finding);
            }
        }
        Ok(findings)
    }

    /// Recomputes stored credit and blocks flushing for every mismatching address.
    pub async fn check_credit_recomputation(&self) -> Result<Vec<Finding>, ReconciliationError> {
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
        let mut findings = Vec::new();
        for id in ids {
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
            let route = routes
                .get(&(route_name.clone(), route_version))
                .ok_or_else(|| {
                    ReconciliationError::Configuration(format!(
                        "missing route `{route_name}` version {route_version}"
                    ))
                })?;
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
                continue;
            }
            store::block_address(
                &self.pool,
                deposit.chain_id,
                deposit.address_id,
                CheckName::CreditRecomputation.code(),
                "stored credit disagrees with deterministic recomputation",
            )
            .await?;
            findings.push(Finding::new(
                CheckName::CreditRecomputation,
                subjects([
                    ("deposit_id", deposit.id.to_string()),
                    ("address_id", deposit.address_id.to_string()),
                    ("chain_id", deposit.chain_id.to_string()),
                ]),
                json!({"credit_minor": expected.to_string()}),
                json!({"credit_minor": stored.value().to_string()}),
                false,
                false,
            )?);
        }
        Ok(findings)
    }

    /// Links deposits to the earliest later confirmed flush by stored log position.
    pub async fn check_missing_flush_links(&self) -> Result<Vec<Finding>, ReconciliationError> {
        let rows = sqlx::query(
            r#"
            SELECT d.id AS deposit_id, d.state, candidate.flush_id
            FROM deposits d
            JOIN LATERAL (
                SELECT f.flush_id
                FROM flushed f
                JOIN flushes x ON x.id = f.flush_id
                WHERE f.address_id = d.address_id
                  AND x.status = 'confirmed'
                  AND x.token = d.asset_contract
                  AND (d.block_number, d.log_index) < (f.block_number, f.log_index)
                ORDER BY f.block_number, f.log_index, f.flush_id
                LIMIT 1
            ) candidate ON true
            WHERE d.flush_id IS NULL
            ORDER BY d.id
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let mut findings = Vec::new();
        for row in rows {
            let deposit_id: Uuid = row.try_get("deposit_id")?;
            let flush_id: Uuid = row.try_get("flush_id")?;
            let state: String = row.try_get("state")?;
            let repaired = link_deposit(&self.pool, deposit_id, flush_id, &state).await?;
            if repaired {
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
        Ok(findings)
    }

    /// Compares address balances and treasury inflow event totals with the durable ledger.
    pub async fn check_custody_balances(&self) -> Result<Vec<Finding>, ReconciliationError> {
        let mut findings = Vec::new();
        for route in self.latest_asset_routes() {
            let chain = self.chain(route.chain.chain_id)?;
            let addresses = db::list_chain_addresses(&self.pool, route.chain.chain_id).await?;
            let physical = addresses
                .iter()
                .map(|address| address.address)
                .collect::<Vec<_>>();
            let balances = chain
                .token_balances(route.asset.contract, &physical)
                .await?;
            if balances.len() != addresses.len() {
                return Err(ReconciliationError::Invariant(
                    "balance response length did not match address count",
                ));
            }
            let totals = store::address_totals(
                &self.pool,
                route.chain.chain_id,
                &format!("{:#x}", route.asset.contract),
            )
            .await?
            .into_iter()
            .map(|(id, deposits, flushed)| (id, (deposits, flushed)))
            .collect::<BTreeMap<_, _>>();
            for (address, observed) in addresses.iter().zip(balances) {
                let (deposits, flushed) =
                    totals
                        .get(&address.id)
                        .ok_or(ReconciliationError::Invariant(
                            "address accounting total is missing",
                        ))?;
                let deposits = parse_u256(deposits)?;
                let flushed = parse_u256(flushed)?;
                let expected = deposits.checked_sub(flushed).unwrap_or(U256::MAX);
                if expected != observed {
                    findings.push(Finding::new(
                        CheckName::CustodyBalance,
                        subjects([
                            ("chain_id", route.chain.chain_id.to_string()),
                            ("address_id", address.id.to_string()),
                            ("token", format!("{:#x}", route.asset.contract)),
                        ]),
                        json!({"balance_atomic": expected.to_string()}),
                        json!({"balance_atomic": observed.to_string()}),
                        false,
                        false,
                    )?);
                }
            }
            let finalized = chain.finalized_head().await?;
            let flushed_total = chain
                .flushed_total(
                    route.chain.contracts.forwarder_factory,
                    route.asset.contract,
                    0,
                    finalized,
                )
                .await?;
            let treasury_inflow = treasury_inflow_total(
                chain,
                route.chain.contracts.treasury,
                route.asset.contract,
                finalized,
            )
            .await?;
            if treasury_inflow != flushed_total {
                findings.push(Finding::new(
                    CheckName::CustodyBalance,
                    subjects([
                        ("chain_id", route.chain.chain_id.to_string()),
                        ("treasury", format!("{:#x}", route.chain.contracts.treasury)),
                        ("token", format!("{:#x}", route.asset.contract)),
                    ]),
                    json!({"flushed_event_total": flushed_total.to_string()}),
                    json!({"treasury_inflow_total": treasury_inflow.to_string()}),
                    false,
                    false,
                )?);
            }
        }
        Ok(findings)
    }

    /// Verifies every stored salt with the on-chain factory and freezes mismatching chains.
    pub async fn check_address_derivation(&self) -> Result<Vec<Finding>, ReconciliationError> {
        let mut findings = Vec::new();
        for (chain_id, factory) in self.chain_factories()? {
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
        }
        Ok(findings)
    }

    /// GETs every deposit at or beyond cleared before a restored service resumes.
    pub async fn check_post_restore_settlements(
        &self,
    ) -> Result<Vec<Finding>, ReconciliationError> {
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
        let mut findings = Vec::new();
        for id in ids {
            if let Some(finding) = self.reconcile_product_answer(id, true).await? {
                findings.push(finding);
            }
        }
        Ok(findings)
    }

    async fn reconcile_product_answer(
        &self,
        deposit_id: Uuid,
        post_restore: bool,
    ) -> Result<Option<Finding>, ReconciliationError> {
        let deposit = db::get_deposit(&self.pool, deposit_id).await?.ok_or(
            ReconciliationError::Invariant("settlement deposit is missing"),
        )?;
        let row = sqlx::query(
            r#"
            SELECT a.external_id, a.product_id, p.settlement_url,
                   COALESCE(s.key, 'deposit:' || d.id::text) AS key,
                   s.deposit_id IS NOT NULL AS has_settlement
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
        let settlement_url: String = row.try_get("settlement_url")?;
        let key: String = row.try_get("key")?;
        let has_settlement: bool = row.try_get("has_settlement")?;
        let answer = self.settlement.get_by_key(&settlement_url, &key).await?;
        let check = if post_restore {
            CheckName::PostRestoreSettlement
        } else {
            CheckName::SentSettlement
        };
        let Some(answer) = answer else {
            return Ok(Some(Finding::new(
                check,
                subjects([("deposit_id", deposit_id.to_string()), ("key", key)]),
                json!({"product_answer": "terminal"}),
                json!({"product_answer": null}),
                false,
                post_restore,
            )?));
        };
        let answer_kind = settlement_answer_kind(&answer);
        if let Err(error) = validate_answer_identity(&deposit, &external_id, &answer) {
            return Ok(Some(Finding::new(
                check,
                subjects([("deposit_id", deposit_id.to_string()), ("key", key)]),
                json!({"product_answer": "identity_verified"}),
                json!({"product_answer": answer_kind, "error": error.code()}),
                false,
                post_restore,
            )?));
        }
        if !has_settlement {
            let Some(payload) = answer_payload(&answer) else {
                return Ok(Some(Finding::new(
                    check,
                    subjects([("deposit_id", deposit_id.to_string()), ("key", key)]),
                    json!({"settlement_intent": "recoverable_terminal_answer"}),
                    json!({"settlement_intent": null, "product_answer": answer_kind}),
                    false,
                    post_restore,
                )?));
            };
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
                    return Ok(Some(Finding::new(
                        check,
                        subjects([("deposit_id", deposit_id.to_string()), ("key", key)]),
                        json!({"product_answer": "adopted"}),
                        json!({"product_answer": answer_kind, "error": error.code()}),
                        false,
                        post_restore,
                    )?));
                }
            };
        let repaired =
            apply_reconciliation_result(&self.pool, &deposit, &result, post_restore).await?;
        let terminal_compatible = authoritative_target(&deposit, &result.outcome)
            .is_some_and(|target| target == state_code(deposit.state) || repaired);
        let incomplete = post_restore && !terminal_compatible;
        Ok(Some(Finding::new(
            check,
            subjects([("deposit_id", deposit_id.to_string()), ("key", key)]),
            json!({"local_state": expected_state_for_answer(&result.outcome)}),
            json!({"local_state": state_code(deposit.state), "product_answer": answer_kind}),
            repaired || terminal_compatible,
            incomplete,
        )?))
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

fn block_windows(from: u64, to: u64) -> Result<Vec<(u64, u64)>, ReconciliationError> {
    let mut windows = Vec::new();
    let mut start = from;
    loop {
        let end = start
            .saturating_add(MAX_SCAN_WINDOW.saturating_sub(1))
            .min(to);
        windows.push((start, end));
        if end == to {
            break;
        }
        start = end.checked_add(1).ok_or(ReconciliationError::Invariant(
            "reconciliation block range overflowed",
        ))?;
    }
    Ok(windows)
}

fn subjects<const N: usize>(pairs: [(&str, String); N]) -> BTreeMap<String, String> {
    pairs
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value))
        .collect()
}

fn parse_u256(value: &str) -> Result<U256, ReconciliationError> {
    value
        .parse()
        .map_err(|_| ReconciliationError::Invariant("stored atomic total is invalid"))
}

async fn treasury_inflow_total(
    chain: &Arc<dyn ReconciliationChain>,
    treasury: Address,
    token: Address,
    finalized: u64,
) -> Result<U256, ReconciliationError> {
    let mut total = U256::ZERO;
    for (from_block, to_block) in block_windows(0, finalized)? {
        let logs = chain
            .transfer_logs_to(std::slice::from_ref(&treasury), from_block, to_block)
            .await?;
        for log in logs {
            if log.token != token || log.to != treasury {
                continue;
            }
            total = total
                .checked_add(log.amount.value())
                .ok_or(ReconciliationError::Invariant(
                    "treasury inflow total overflowed U256",
                ))?;
        }
    }
    Ok(total)
}

async fn link_deposit(
    pool: &PgPool,
    deposit_id: Uuid,
    flush_id: Uuid,
    state: &str,
) -> Result<bool, ReconciliationError> {
    let mut transaction = pool.begin().await?;
    let target = if state == "credited" { "swept" } else { state };
    let updated = sqlx::query(
        r#"
        UPDATE deposits
        SET flush_id = $2,
            state = $3,
            attempt = CASE WHEN state = 'credited' THEN 0 ELSE attempt END,
            lease_token = CASE WHEN state = 'credited' THEN NULL ELSE lease_token END,
            lease_until = CASE WHEN state = 'credited' THEN NULL ELSE lease_until END,
            updated_at = now()
        WHERE id = $1 AND flush_id IS NULL
        "#,
    )
    .bind(deposit_id)
    .bind(flush_id)
    .bind(target)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    if updated && state == "credited" {
        sqlx::query(
            r#"
            INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
            VALUES ($1, $2, 'credited', 'swept', 0, $3)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(deposit_id)
        .bind(json!({"outcome": "advance", "flush_id": flush_id, "source": "reconciler"}))
        .execute(&mut *transaction)
        .await?;
    }
    transaction.commit().await?;
    Ok(updated)
}

async fn apply_reconciliation_result(
    pool: &PgPool,
    deposit: &db::Deposit,
    result: &StepResult,
    post_restore: bool,
) -> Result<bool, ReconciliationError> {
    let Some(target) = authoritative_target(deposit, &result.outcome) else {
        return Ok(false);
    };
    if !post_restore && deposit.state != DepositState::Cleared {
        return Ok(false);
    }
    if target == state_code(deposit.state) {
        return Ok(false);
    }
    let reason = (target == "rejected").then_some(RejectReason::ProductRefused.code());
    let mut transaction = pool.begin().await?;
    let updated = sqlx::query(
        r#"
        UPDATE deposits
        SET state = $2, reason = $3, attempt = 0, next_attempt_at = now(),
            lease_token = NULL, lease_until = NULL, updated_at = now()
        WHERE id = $1 AND state = $4
          AND (lease_until IS NULL OR lease_until <= now())
        "#,
    )
    .bind(deposit.id)
    .bind(target)
    .bind(reason)
    .bind(state_code(deposit.state))
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        == 1;
    if updated {
        sqlx::query(
            r#"
            INSERT INTO transitions (id, deposit_id, from_state, to_state, attempt, evidence)
            VALUES ($1, $2, $3, $4, 0, $5)
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(deposit.id)
        .bind(state_code(deposit.state))
        .bind(target)
        .bind(&result.evidence)
        .execute(&mut *transaction)
        .await?;
        for event in &result.events {
            sqlx::query(
                "INSERT INTO outbox (id, event_type, payload, next_attempt_at) VALUES ($1, $2, $3, $4) ON CONFLICT (id) DO NOTHING",
            )
            .bind(event.id)
            .bind(&event.event_type)
            .bind(&event.payload)
            .bind(event.next_attempt_at)
            .execute(&mut *transaction)
            .await?;
        }
    }
    transaction.commit().await?;
    Ok(updated)
}

fn authoritative_target(deposit: &db::Deposit, outcome: &StepOutcome) -> Option<&'static str> {
    match outcome {
        StepOutcome::Advance => Some(if deposit.flush_id.is_some() {
            "swept"
        } else {
            "credited"
        }),
        StepOutcome::Reject(RejectReason::ProductRefused) => Some("rejected"),
        StepOutcome::Reject(_)
        | StepOutcome::Retry { .. }
        | StepOutcome::Wait { .. }
        | StepOutcome::AdoptProductAnswer { .. } => None,
    }
}

fn answer_payload(answer: &SettlementAnswer) -> Option<Value> {
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

fn expected_state_for_answer(outcome: &StepOutcome) -> &'static str {
    match outcome {
        StepOutcome::Advance => "credited",
        StepOutcome::Reject(_) => "rejected",
        StepOutcome::AdoptProductAnswer { credited: true, .. } => "credited",
        StepOutcome::AdoptProductAnswer {
            credited: false, ..
        } => "rejected",
        StepOutcome::Wait { .. } | StepOutcome::Retry { .. } => "terminal_product_answer",
    }
}

const fn state_code(state: DepositState) -> &'static str {
    match state {
        DepositState::Detected => "detected",
        DepositState::Confirmed => "confirmed",
        DepositState::Cleared => "cleared",
        DepositState::Credited => "credited",
        DepositState::Swept => "swept",
        DepositState::Rejected => "rejected",
    }
}
