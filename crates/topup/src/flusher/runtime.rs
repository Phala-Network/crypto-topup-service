//! Scheduled flusher runtime wiring for the unified service process.

use std::collections::BTreeMap;
use std::io;
use std::num::NonZeroU32;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use alloy_primitives::{Address, U256};
use async_trait::async_trait;
use chrono::Utc;
use croner::Cron;
use sqlx::PgPool;
use tokio::time::{Instant, MissedTickBehavior, interval, sleep_until};
use tokio_util::sync::CancellationToken;
use topup_adapters::pricing::PriceSource as _;
use topup_adapters::pricing::coinmetrics::CoinMetrics;
use topup_adapters::signer::actor::SignerHandle;
use topup_core::money::ScaledPrice;
use topup_core::route::RouteFile;
use tracing::Instrument as _;

use super::{
    AlertSink, FlushAlert, Flusher, FlusherPolicy, OperatorRole, Planner, PriceError, PriceSource,
    RunResult,
};
use crate::observability::FlushPlanningOutcome;
use crate::routes::RouteSet;

/// Interval between confirmation, replacement, operator-role, and operator-gas maintenance
/// iterations.
const MAINTENANCE_INTERVAL: Duration = Duration::from_secs(5);

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
    /// Creates a task whose schedule comes from the route's chain policy.
    pub fn new(
        route: RouteFile,
        planner: Planner,
        flusher: Flusher,
        alerts: Arc<dyn AlertSink>,
    ) -> Result<Self, String> {
        let schedule = Cron::from_str(&route.chain.flush.schedule)
            .map_err(|error| format!("invalid flush schedule for `{}`: {error}", route.route))?;
        Ok(Self {
            route,
            planner,
            flusher,
            alerts,
            schedule,
            maintenance_interval: MAINTENANCE_INTERVAL,
        })
    }

    /// Replaces the maintenance interval, for tests that observe several ticks.
    #[must_use]
    pub const fn with_maintenance_interval(mut self, interval: Duration) -> Self {
        self.maintenance_interval = interval;
        self
    }

    /// Returns `{chain_id}:{route}`, the task's log name.
    #[must_use]
    pub fn instance(&self) -> String {
        format!("{}:{}", self.route.chain.chain_id, self.route.route)
    }

    /// Runs startup recovery, scheduled planning, and periodic lifecycle maintenance.
    ///
    /// New flushes are planned and sent only while the configured operator holds
    /// `OPERATOR_ROLE` on the factory, so an operator-key version is used only after the admin
    /// Safe has granted it and stops being used as soon as the role is revoked. The role is
    /// checked on every maintenance tick, the first of which is immediate; without it, the task
    /// keeps maintaining already sent flushes.
    pub async fn run(self, cancellation: CancellationToken) {
        let monitor = crate::observability::CronMonitor::flush_planning(
            &self.route.route,
            &self.route.chain.flush.schedule,
        );
        if monitor.is_none() {
            tracing::warn!(
                route = %self.route.route,
                "flush schedule is not a five-field crontab; Sentry Crons cannot monitor it"
            );
        }
        let mut authorized = false;
        let startup_span = crate::observability::flush_action_span(
            self.route.chain.chain_id,
            &self.route.route,
            "startup_recovery",
            0,
        );
        if let Err(error) = self
            .flusher
            .maintain_sent(&self.route)
            .instrument(startup_span)
            .await
        {
            tracing::error!(%error, route = %self.route.route, "flusher startup recovery failed");
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
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = maintenance.tick() => {
                    authorized = self.operator_authorized(authorized).await;
                    if authorized {
                        self.check_operator_gas().await;
                    }
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
                    if let Err(error) = result {
                        tracing::error!(%error, route = %self.route.route, "flush maintenance failed");
                    }
                }
                () = sleep_until(next_plan) => {
                    authorized = self.operator_authorized(authorized).await;
                    let mut planned = false;
                    let mut outcome = (FlushPlanningOutcome::OperatorNotAuthorized, None);
                    if authorized {
                        let plan_span = crate::observability::flush_action_span(
                            self.route.chain.chain_id,
                            &self.route.route,
                            "planning",
                            0,
                        );
                        match self.planner.plan(&self.route).instrument(plan_span).await {
                            Ok(flush_id) => {
                                planned = true;
                                outcome = (FlushPlanningOutcome::Idle, None);
                                if flush_id.is_some() {
                                    outcome.0 = FlushPlanningOutcome::Planned;
                                }
                                tracing::info!(route = %self.route.route, ?flush_id, "flush planning completed");
                                let send_span = crate::observability::flush_action_span(
                                    self.route.chain.chain_id,
                                    &self.route.route,
                                    "planned_send",
                                    0,
                                );
                                if let Err(error) = self.flusher.run_once(&self.route).instrument(send_span).await {
                                    tracing::error!(%error, route = %self.route.route, "planned flush send failed");
                                    outcome = (FlushPlanningOutcome::SendFailed, Some(error.to_string()));
                                }
                            }
                            Err(error) => {
                                tracing::error!(%error, route = %self.route.route, "flush planning failed");
                                outcome = (FlushPlanningOutcome::Failed, Some(error.to_string()));
                            }
                        }
                    }
                    crate::observability::record_flush_planning(&self.route.route, outcome.0, outcome.1);
                    if let Some(monitor) = &monitor {
                        monitor.check_in(planned);
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

    /// Alerts when the operator's native balance is below the route's gas reserve.
    async fn check_operator_gas(&self) {
        match self.flusher.operator_balance().await {
            Ok((operator, balance)) => report_operator_gas(&self.route, operator, balance),
            Err(error) => tracing::warn!(
                %error,
                chain_id = self.route.chain.chain_id,
                route = %self.route.route,
                "flusher operator balance check failed"
            ),
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
                    tags.alert = "OperatorRoleMissing",
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

/// Raises `TopupOperatorGasReserveLow` when `balance` is below `chain.flush.min_operator_balance_wei`.
fn report_operator_gas(route: &RouteFile, operator: Address, balance: U256) {
    let reserve = route.chain.flush.min_operator_balance_wei.value();
    if balance < reserve {
        tracing::warn!(
            tags.alert = "TopupOperatorGasReserveLow",
            tags.chain = route.chain.chain_id,
            tags.route = %route.route,
            %operator,
            balance_wei = %balance,
            reserve_wei = %reserve,
            "flusher operator native balance is below its gas reserve; refill it from the Finance Safe"
        );
    }
}

/// Builds one task for the newest attested version of each chain/token route.
///
/// Each task signs with the operator key version from its route's chain configuration;
/// `operator_signer` starts one signer per distinct version.
pub fn configure_tasks(
    pool: PgPool,
    routes: &RouteSet,
    mut operator_signer: impl FnMut(NonZeroU32) -> io::Result<SignerHandle>,
) -> Result<Vec<FlusherTask>, String> {
    let latest = latest_routes(routes)?;
    let mut signers = BTreeMap::new();
    let mut tasks = Vec::with_capacity(latest.len());
    for route in latest {
        let chain_id = route.chain.chain_id;
        let chain = Arc::clone(
            routes
                .provider(chain_id, 0)
                .map_err(|error| format!("flusher route `{}`: {error}", route.route))?,
        );
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
        let prices: Arc<dyn PriceSource> = Arc::new(CoinMetricsPriceSource::for_route(&route)?);
        let alerts: Arc<dyn AlertSink> = Arc::new(TracingAlertSink);
        let planner = Planner::new(
            pool.clone(),
            chain.clone(),
            signer.clone(),
            prices,
            alerts.clone(),
        );
        let policy = FlusherPolicy {
            replacement_bps: route.chain.flush.replacement_bps,
            max_fee_per_gas: u128::from(route.chain.flush.max_fee_per_gas_wei),
            ..FlusherPolicy::default()
        };
        let flusher = Flusher::new(pool.clone(), chain, signer, alerts.clone(), policy);
        tasks.push(FlusherTask::new(route, planner, flusher, alerts)?);
    }
    Ok(tasks)
}

/// Selects the newest route version per chain/token and requires one operator key per chain.
fn latest_routes(routes: &RouteSet) -> Result<Vec<RouteFile>, String> {
    routes.operator_keys()?;
    Ok(routes.current().cloned().collect())
}

/// Coin Metrics `ReferenceRateUSD` sources for the route token and the chain's native gas asset.
struct CoinMetricsPriceSource(BTreeMap<String, CoinMetrics>);

impl CoinMetricsPriceSource {
    fn for_route(route: &RouteFile) -> Result<Self, String> {
        [
            &route.pricing.primary.asset,
            &route.chain.flush.native_price_asset,
        ]
        .into_iter()
        .map(|asset| {
            CoinMetrics::new(asset.clone())
                .map(|source| (asset.clone(), source))
                .map_err(|error| format!("failed to configure Coin Metrics for `{asset}`: {error}"))
        })
        .collect::<Result<_, _>>()
        .map(Self)
    }
}

#[async_trait]
impl PriceSource for CoinMetricsPriceSource {
    async fn price_usd(&self, asset: &str) -> Result<ScaledPrice, PriceError> {
        let source = self
            .0
            .get(asset)
            .ok_or_else(|| PriceError::UnconfiguredAsset(asset.to_owned()))?;
        source.observe().await.map(|observation| observation.price)
    }
}

struct TracingAlertSink;

impl AlertSink for TracingAlertSink {
    fn emit(&self, alert: FlushAlert) {
        tracing::warn!(tags.alert = alert.name(), ?alert, "flusher alert");
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
    use alloy_primitives::{Address, U256};
    use topup_core::money::AtomicAmount;
    use topup_core::route::RouteFile;
    use tracing_test::traced_test;

    use super::{latest_routes, report_operator_gas};
    use crate::routes::RouteSet;

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
        let routes = |routes| RouteSet::new(routes).expect("routes load");
        let rotated = latest_routes(&routes(vec![
            route("a", 1, 1, 1),
            route("a", 2, 1, 2),
            route("b", 1, 2, 2),
        ]))
        .expect("historical versions may keep the previous operator key");
        assert_eq!(
            rotated
                .iter()
                .map(|route| (route.route.as_str(), route.version))
                .collect::<Vec<_>>(),
            [("a", 2), ("b", 1)]
        );

        let error = latest_routes(&routes(vec![route("a", 2, 1, 2), route("b", 1, 2, 1)]))
            .expect_err("current routes on one chain must agree");
        assert!(error.contains("operator_key_version"), "{error}");

        let mut other_chain = route("b", 1, 2, 1);
        other_chain.chain.chain_id = 10;
        latest_routes(&routes(vec![route("a", 2, 1, 2), other_chain]))
            .expect("different chains may rotate independently");
    }

    #[test]
    #[traced_test]
    fn operator_gas_alert_fires_only_below_the_reserve() {
        let mut route = route("a", 1, 1, 1);
        route.chain.flush.min_operator_balance_wei = AtomicAmount::new(U256::from(1_000));

        report_operator_gas(&route, Address::ZERO, U256::from(1_000));
        assert!(!logs_contain("TopupOperatorGasReserveLow"));

        report_operator_gas(&route, Address::ZERO, U256::from(999));
        assert!(logs_contain("TopupOperatorGasReserveLow"));
    }
}
