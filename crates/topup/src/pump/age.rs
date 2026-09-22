use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_core::deposit::DepositState;
use topup_core::route::{RouteFile, StuckAfterConfig};
use uuid::Uuid;

/// Route-version-indexed thresholds for state-age alerts.
#[derive(Clone, Debug)]
pub struct AgeAlertConfig {
    thresholds: BTreeMap<(String, u64), StuckAfterConfig>,
}

impl AgeAlertConfig {
    /// Builds alert thresholds from validated route files.
    pub fn from_routes(routes: &[RouteFile]) -> Result<Self, AgeAlertConfigError> {
        let mut thresholds = BTreeMap::new();
        for route in routes {
            let key = (route.route.clone(), route.version);
            if thresholds
                .insert(key.clone(), route.alerts.stuck_after_s.clone())
                .is_some()
            {
                return Err(AgeAlertConfigError {
                    route: key.0,
                    version: key.1,
                });
            }
        }
        Ok(Self { thresholds })
    }

    fn threshold(&self, route: &str, version: u64, state: DepositState) -> Option<u64> {
        let stuck_after = self.thresholds.get(&(route.to_owned(), version))?;
        match state {
            DepositState::Detected => Some(stuck_after.detected),
            DepositState::Confirmed => Some(stuck_after.confirmed),
            DepositState::Cleared => Some(stuck_after.cleared),
            DepositState::Credited => Some(stuck_after.credited),
            DepositState::Swept | DepositState::Rejected => None,
        }
    }
}

/// Duplicate route/version alert configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgeAlertConfigError {
    route: String,
    version: u64,
}

impl Display for AgeAlertConfigError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "duplicate age alert configuration for route `{}` version {}",
            self.route, self.version
        )
    }
}

impl Error for AgeAlertConfigError {}

/// Minimal in-process metrics emitted by the pump work package.
#[derive(Debug, Default)]
pub struct PumpMetrics {
    stuck_deposit_alerts: AtomicU64,
}

impl PumpMetrics {
    /// Returns the number of stuck-deposit alert observations.
    #[must_use]
    pub fn stuck_deposit_alerts(&self) -> u64 {
        self.stuck_deposit_alerts.load(Ordering::Relaxed)
    }

    fn record_stuck_deposit(&self) {
        self.stuck_deposit_alerts.fetch_add(1, Ordering::Relaxed);
    }
}

/// Periodically finds deposits older than their route's state threshold.
pub struct AgeAlerter {
    pool: PgPool,
    config: AgeAlertConfig,
    metrics: Arc<PumpMetrics>,
    scan_interval: Duration,
}

impl AgeAlerter {
    /// Creates a periodic state-age alerter.
    #[must_use]
    pub fn new(
        pool: PgPool,
        config: AgeAlertConfig,
        metrics: Arc<PumpMetrics>,
        scan_interval: Duration,
    ) -> Self {
        Self {
            pool,
            config,
            metrics,
            scan_interval,
        }
    }

    /// Scans until cancellation, logging failures without stopping the task.
    pub async fn run(&self, cancellation: CancellationToken) {
        let mut ticker = interval(self.scan_interval);
        ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                () = cancellation.cancelled() => return,
                _ = ticker.tick() => {
                    if let Err(error) = self.scan_once().await {
                        tracing::error!(%error, "deposit age alert scan failed");
                    }
                }
            }
        }
    }

    /// Performs one state-age scan and returns the number of alerts emitted.
    pub async fn scan_once(&self) -> Result<u64, sqlx::Error> {
        let rows = sqlx::query_as::<_, DepositAgeRow>(
            r#"
            SELECT
                deposit.id,
                deposit.route,
                deposit.route_version,
                deposit.state,
                COALESCE(
                    (
                        SELECT max(transition.created_at)
                        FROM transitions AS transition
                        WHERE transition.deposit_id = deposit.id
                          AND transition.from_state <> transition.to_state
                          AND transition.to_state = deposit.state
                    ),
                    deposit.created_at
                ) AS entered_at
            FROM deposits AS deposit
            WHERE deposit.state NOT IN ('swept', 'rejected')
            "#,
        )
        .fetch_all(&self.pool)
        .await?;
        let now = Utc::now();
        let mut alert_count = 0_u64;
        for row in rows {
            let Some(route) = row.route.as_deref() else {
                continue;
            };
            let Some(version) = row
                .route_version
                .and_then(|value| u64::try_from(value).ok())
            else {
                continue;
            };
            let Some(state) = parse_active_state(&row.state) else {
                continue;
            };
            let Some(threshold) = self.config.threshold(route, version, state) else {
                continue;
            };
            let age_seconds = now.signed_duration_since(row.entered_at).num_seconds();
            let Ok(threshold_seconds) = i64::try_from(threshold) else {
                continue;
            };
            if age_seconds > threshold_seconds {
                tracing::warn!(
                    deposit_id = %row.id,
                    route,
                    route_version = version,
                    state = ?state,
                    age_seconds,
                    threshold_seconds,
                    "deposit has exceeded its state-age threshold"
                );
                self.metrics.record_stuck_deposit();
                alert_count = alert_count.saturating_add(1);
            }
        }
        Ok(alert_count)
    }
}

#[derive(FromRow)]
struct DepositAgeRow {
    id: Uuid,
    route: Option<String>,
    route_version: Option<i64>,
    state: String,
    entered_at: DateTime<Utc>,
}

fn parse_active_state(state: &str) -> Option<DepositState> {
    match state {
        "detected" => Some(DepositState::Detected),
        "confirmed" => Some(DepositState::Confirmed),
        "cleared" => Some(DepositState::Cleared),
        "credited" => Some(DepositState::Credited),
        "swept" | "rejected" => None,
        _ => None,
    }
}
