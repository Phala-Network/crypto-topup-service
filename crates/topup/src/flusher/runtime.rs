//! Scheduled flusher runtime wiring for the unified service process.

use std::collections::BTreeMap;
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
    AlertSink, AlloyChainClient, FlushAlert, Flusher, FlusherPolicy, Planner, PriceError,
    PriceSource, RunResult,
};
use crate::rpc_provider::configured_provider_url;

/// One configured chain/token flusher task.
pub struct FlusherTask {
    route: RouteFile,
    planner: Planner,
    flusher: Flusher,
    schedule: Cron,
    maintenance_interval: Duration,
}

impl FlusherTask {
    /// Runs startup recovery, scheduled planning, and periodic lifecycle maintenance.
    pub async fn run(self, cancellation: CancellationToken) {
        let instance = format!("{}:{}", self.route.chain.chain_id, self.route.route);
        crate::observability::register_loop("flusher", instance.clone());
        crate::observability::heartbeat("flusher", instance.clone());
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
                    let span = crate::observability::flush_action_span(
                        self.route.chain.chain_id,
                        &self.route.route,
                        "maintenance",
                        0,
                    );
                    match self.flusher.run_once(&self.route).instrument(span).await {
                        Ok(RunResult::Idle) => {}
                        Ok(_) => crate::observability::progress("flusher", instance.clone()),
                        Err(error) => {
                            tracing::error!(%error, route = %self.route.route, "flush maintenance failed");
                        }
                    }
                }
                () = sleep_until(next_plan) => {
                    crate::observability::heartbeat("flusher", instance.clone());
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
}

/// Builds one task for the newest attested version of each chain/token route.
pub fn configure_tasks(
    pool: PgPool,
    routes: &[RouteFile],
    signer: SignerHandle,
) -> Result<Vec<FlusherTask>, String> {
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
    let mut tasks = Vec::with_capacity(latest.len());
    for route in latest.into_values() {
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
        let flusher = Flusher::new(pool.clone(), chain, signer.clone(), alerts, policy);
        let schedule = Cron::from_str(&route.chain.flush.schedule)
            .map_err(|error| format!("invalid flush schedule for `{}`: {error}", route.route))?;
        let maintenance_interval = Duration::from_secs(route.chain.flush.maintenance_interval_s);
        tasks.push(FlusherTask {
            route,
            planner,
            flusher,
            schedule,
            maintenance_interval,
        });
    }
    Ok(tasks)
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
