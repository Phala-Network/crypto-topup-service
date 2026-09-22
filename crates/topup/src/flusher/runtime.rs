//! Scheduled flusher runtime wiring for the unified service process.

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::Utc;
use croner::Cron;
use sqlx::PgPool;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};
use tokio_util::sync::CancellationToken;
use topup_adapters::pricing::native::CoinMetricsUsdClient;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::money::ScaledPrice;
use topup_core::route::RouteFile;
use tracing::Instrument as _;

use super::{
    AlertSink, AlloyChainClient, FlushAlert, Flusher, FlusherPolicy, OperatorRole, Planner,
    PriceError, PriceSource, RunResult,
};
use crate::rpc_provider::configured_provider_url;

/// One configured chain/token flusher task.
pub struct FlusherTask {
    route: RouteFile,
    planner: Planner,
    flusher: Flusher,
    alerts: Arc<dyn AlertSink>,
    schedule: Cron,
    maintenance_interval: Duration,
}

impl FlusherTask {
    /// Creates a task whose schedule and maintenance interval come from the route's chain policy.
    pub fn new(
        route: RouteFile,
        planner: Planner,
        flusher: Flusher,
        alerts: Arc<dyn AlertSink>,
    ) -> Result<Self, String> {
        let schedule = Cron::from_str(&route.chain.flush.schedule)
            .map_err(|error| format!("invalid flush schedule for `{}`: {error}", route.route))?;
        let maintenance_interval = Duration::from_secs(route.chain.flush.maintenance_interval_s);
        Ok(Self {
            route,
            planner,
            flusher,
            alerts,
            schedule,
            maintenance_interval,
        })
    }

    /// Runs startup recovery, scheduled planning, and periodic lifecycle maintenance.
    ///
    /// New flushes are planned and sent only while the configured operator holds
    /// `OPERATOR_ROLE` on the factory, so an operator-key version is used only after the admin
    /// Safe has granted it and stops being used as soon as the role is revoked. The role is
    /// checked on every maintenance tick, the first of which is immediate; without it, the task
    /// keeps maintaining already sent flushes.
    pub async fn run(self, cancellation: CancellationToken) {
        let instance = format!("{}:{}", self.route.chain.chain_id, self.route.route);
        crate::observability::register_loop("flusher", instance.clone());
        crate::observability::heartbeat("flusher", instance.clone());
        let mut authorized = false;
        let startup_span = crate::observability::flush_action_span(
            self.route.chain.chain_id,
            &self.route.route,
            "startup_recovery",
            0,
        );
        match self
            .flusher
            .maintain_sent(&self.route)
            .instrument(startup_span)
            .await
        {
            Ok(Some(_)) => crate::observability::progress("flusher", instance.clone()),
            Ok(None) => {}
            Err(error) => {
                tracing::error!(%error, route = %self.route.route, "flusher startup recovery failed");
            }
        }
        let mut maintenance = interval(self.maintenance_interval);
        maintenance.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut next_plan = match next_deadline(&self.schedule) {
            Ok(deadline) => deadline,
            Err(error) => {
                tracing::error!(%error, route = %self.route.route, "flush schedule failed");
                return;
            }
        };
        loop {
            let wait = self
                .maintenance_interval
                .min(next_plan.saturating_duration_since(Instant::now()));
            crate::observability::waiting("flusher", instance.clone(), wait);
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = maintenance.tick() => {
                    crate::observability::heartbeat("flusher", instance.clone());
                    authorized = self.operator_authorized(authorized).await;
                    let span = crate::observability::flush_action_span(
                        self.route.chain.chain_id,
                        &self.route.route,
                        "maintenance",
                        0,
                    );
                    let result = if authorized {
                        self.flusher.run_once(&self.route).instrument(span).await
                    } else {
                        self.flusher
                            .maintain_sent(&self.route)
                            .instrument(span)
                            .await
                            .map(|result| result.unwrap_or(RunResult::Idle))
                    };
                    match result {
                        Ok(RunResult::Idle) => {}
                        Ok(_) => crate::observability::progress("flusher", instance.clone()),
                        Err(error) => {
                            tracing::error!(%error, route = %self.route.route, "flush maintenance failed");
                        }
                    }
                }
                () = sleep_until(next_plan) => {
                    crate::observability::heartbeat("flusher", instance.clone());
                    authorized = self.operator_authorized(authorized).await;
                    if authorized {
                        let plan_span = crate::observability::flush_action_span(
                            self.route.chain.chain_id,
                            &self.route.route,
                            "planning",
                            0,
                        );
                        match self.planner.plan(&self.route).instrument(plan_span).await {
                            Ok(flush_id) => {
                                if flush_id.is_some() {
                                    crate::observability::progress("flusher", instance.clone());
                                }
                                tracing::info!(route = %self.route.route, ?flush_id, "flush planning completed");
                                let send_span = crate::observability::flush_action_span(
                                    self.route.chain.chain_id,
                                    &self.route.route,
                                    "planned_send",
                                    0,
                                );
                                match self.flusher.run_once(&self.route).instrument(send_span).await {
                                    Ok(RunResult::Idle) => {}
                                    Ok(_) => crate::observability::progress("flusher", instance.clone()),
                                    Err(error) => {
                                        tracing::error!(%error, route = %self.route.route, "planned flush send failed");
                                    }
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, route = %self.route.route, "flush planning failed");
                            }
                        }
                    }
                    next_plan = match next_deadline(&self.schedule) {
                        Ok(deadline) => deadline,
                        Err(error) => {
                            tracing::error!(%error, route = %self.route.route, "flush schedule failed");
                            return;
                        }
                    };
                }
            }
        }
    }

    /// Checks `OPERATOR_ROLE`, keeping `current` when the check itself fails.
    async fn operator_authorized(&self, current: bool) -> bool {
        let chain_id = self.route.chain.chain_id;
        let route = self.route.route.as_str();
        let operator_key_version = self.route.chain.operator_key_version;
        let factory = self.route.chain.contracts.forwarder_factory;
        match self.flusher.operator_role(&self.route).await {
            Ok(OperatorRole {
                operator,
                granted: true,
            }) => {
                if !current {
                    tracing::info!(
                        chain_id,
                        route,
                        operator_key_version,
                        %operator,
                        %factory,
                        "flusher operator holds OPERATOR_ROLE"
                    );
                }
                true
            }
            Ok(OperatorRole {
                operator,
                granted: false,
            }) => {
                tracing::error!(
                    chain_id,
                    route,
                    operator_key_version,
                    %operator,
                    %factory,
                    "flusher paused: the configured operator does not hold OPERATOR_ROLE on the \
                     factory; grant it from the admin Safe"
                );
                self.alerts.emit(FlushAlert::OperatorRoleMissing {
                    chain_id,
                    factory,
                    operator,
                    operator_key_version,
                });
                false
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    chain_id,
                    route,
                    operator_key_version,
                    "flusher operator role check failed"
                );
                current
            }
        }
    }
}

/// Builds one task for the newest attested version of each chain/token route.
///
/// Each task signs with the operator key version from its route's chain configuration;
/// `operator_signer` starts one signer per distinct version.
pub fn configure_tasks(
    pool: PgPool,
    routes: &[RouteFile],
    mut operator_signer: impl FnMut(NonZeroU32) -> io::Result<SignerHandle>,
) -> Result<Vec<FlusherTask>, String> {
    let latest = latest_routes(routes)?;
    let mut signers = BTreeMap::new();
    let mut tasks = Vec::with_capacity(latest.len());
    for route in latest {
        let provider = route
            .chain
            .rpc_providers
            .first()
            .ok_or_else(|| format!("route `{}` has no RPC provider", route.route))?;
        let url = configured_provider_url(provider).map_err(|environment| {
            format!(
                "{environment} is required for flusher route `{}`",
                route.route
            )
        })?;
        let url = crate::observability::Redacted::parse(&url).map_err(|_| {
            format!(
                "flusher route `{}` has an invalid provider URL",
                route.route
            )
        })?;
        let version = route
            .chain
            .operator_key_version()
            .map_err(|error| error.to_string())?;
        let signer = match signers.get(&version) {
            Some(signer) => SignerHandle::clone(signer),
            None => {
                let signer = operator_signer(version).map_err(|error| {
                    format!("failed to start the operator/v{version} signer: {error}")
                })?;
                signers.insert(version, signer.clone());
                signer
            }
        };
        let timeout = Duration::from_millis(route.chain.flush.rpc_timeout_ms);
        let batch_size = usize::try_from(route.chain.flush.balance_batch_size)
            .map_err(|_| "flush balance batch size exceeds usize".to_owned())?;
        let chain = Arc::new(
            AlloyChainClient::connect_http_with_policy(url.expose().as_str(), timeout, batch_size)
                .map_err(|_| format!("failed to configure flusher provider {url}"))?,
        );
        let prices: Arc<dyn PriceSource> =
            Arc::new(CoinMetricsPriceSource(CoinMetricsUsdClient::new(timeout)?));
        let alerts: Arc<dyn AlertSink> = Arc::new(TracingAlertSink);
        let planner = Planner::new(
            pool.clone(),
            chain.clone(),
            signer.clone(),
            prices,
            alerts.clone(),
        );
        let policy = FlusherPolicy {
            replacement_after_blocks: route.chain.flush.replacement_after_blocks,
            replacement_bps: route.chain.flush.replacement_bps,
            max_fee_per_gas: u128::from(route.chain.flush.max_fee_per_gas_wei),
            gas_limit_bps: route.chain.flush.gas_limit_bps,
            recovery_scan_blocks: route.chain.flush.recovery_scan_blocks,
        };
        let flusher = Flusher::new(pool.clone(), chain, signer, alerts.clone(), policy);
        tasks.push(FlusherTask::new(route, planner, flusher, alerts)?);
    }
    Ok(tasks)
}

/// Selects the newest route version per chain/token and requires one operator key per chain.
fn latest_routes(routes: &[RouteFile]) -> Result<Vec<RouteFile>, String> {
    let mut latest = BTreeMap::new();
    for route in routes {
        latest
            .entry((route.chain.chain_id, route.asset.contract))
            .and_modify(|current: &mut RouteFile| {
                if route.version > current.version {
                    *current = route.clone();
                }
            })
            .or_insert_with(|| route.clone());
    }
    let mut versions = BTreeMap::new();
    for route in latest.values() {
        let chain_id = route.chain.chain_id;
        let version = route.chain.operator_key_version;
        if let Some((other, other_version)) = versions.insert(chain_id, (&route.route, version))
            && other_version != version
        {
            return Err(format!(
                "current routes `{other}` and `{}` on chain {chain_id} use operator key versions \
                 {other_version} and {version}; they must share one operator_key_version",
                route.route
            ));
        }
    }
    Ok(latest.into_values().collect())
}

struct CoinMetricsPriceSource(CoinMetricsUsdClient);

#[async_trait]
impl PriceSource for CoinMetricsPriceSource {
    async fn price_usd(&self, asset: &str) -> Result<ScaledPrice, PriceError> {
        self.0.price_usd(asset).await.map_err(PriceError)
    }
}

struct TracingAlertSink;

impl AlertSink for TracingAlertSink {
    fn emit(&self, alert: FlushAlert) {
        tracing::warn!(?alert, "flusher alert");
    }
}

fn next_deadline(schedule: &Cron) -> Result<Instant, String> {
    let now = Utc::now();
    let next = schedule
        .find_next_occurrence(&now, false)
        .map_err(|error| error.to_string())?;
    let delay = next
        .signed_duration_since(now)
        .to_std()
        .map_err(|error| format!("flush schedule returned a past occurrence: {error}"))?;
    Instant::now()
        .checked_add(delay)
        .ok_or_else(|| "flush schedule deadline exceeds Tokio instant range".to_owned())
}

#[cfg(test)]
mod tests {
    use alloy_primitives::Address;
    use topup_core::route::RouteFile;

    use super::latest_routes;

    fn route(name: &str, version: u64, token: u8, operator_key_version: u32) -> RouteFile {
        let mut route: RouteFile =
            serde_saphyr::from_str(include_str!("../../tests/fixtures/phala-cloud-pha.yaml"))
                .expect("fixture route parses");
        route.route = name.to_owned();
        route.version = version;
        route.asset.contract = Address::from([token; 20]);
        route.chain.operator_key_version = operator_key_version;
        route
    }

    #[test]
    fn current_routes_on_one_chain_must_share_the_operator_key_version() {
        let rotated = latest_routes(&[
            route("a", 1, 1, 1),
            route("a", 2, 1, 2),
            route("b", 1, 2, 2),
        ])
        .expect("historical versions may keep the previous operator key");
        assert_eq!(
            rotated
                .iter()
                .map(|route| (route.route.as_str(), route.version))
                .collect::<Vec<_>>(),
            [("a", 2), ("b", 1)]
        );

        let error = latest_routes(&[route("a", 2, 1, 2), route("b", 1, 2, 1)])
            .expect_err("current routes on one chain must agree");
        assert!(error.contains("operator_key_version"), "{error}");

        let mut other_chain = route("b", 1, 2, 1);
        other_chain.chain.chain_id = 10;
        latest_routes(&[route("a", 2, 1, 2), other_chain])
            .expect("different chains may rotate independently");
    }
}
