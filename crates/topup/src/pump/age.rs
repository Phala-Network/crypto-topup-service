use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use sqlx::{FromRow, PgPool};
use tokio::sync::Mutex;
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;
use topup_core::deposit::DepositState;
use topup_core::route::{RouteFile, StuckAfterConfig};
use uuid::Uuid;

const DEFAULT_REMINDER_INTERVAL: Duration = Duration::from_secs(60 * 60);

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

/// Periodically finds deposits older than their route's state threshold.
pub struct AgeAlerter {
    pool: PgPool,
    config: AgeAlertConfig,
    scan_interval: Duration,
    reminder_interval: Duration,
    alerts: Mutex<BTreeMap<Uuid, AlertRecord>>,
}

impl AgeAlerter {
    /// Creates a periodic state-age alerter.
    #[must_use]
    pub fn new(pool: PgPool, config: AgeAlertConfig, scan_interval: Duration) -> Self {
        Self::with_reminder_interval(pool, config, scan_interval, DEFAULT_REMINDER_INTERVAL)
    }

    /// Creates an alerter with an explicit reminder interval.
    #[must_use]
    pub fn with_reminder_interval(
        pool: PgPool,
        config: AgeAlertConfig,
        scan_interval: Duration,
        reminder_interval: Duration,
    ) -> Self {
        Self {
            pool,
            config,
            scan_interval,
            reminder_interval,
            alerts: Mutex::new(BTreeMap::new()),
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
        let alert_now = Instant::now();
        let mut alert_count = 0_u64;
        let mut observed_states = BTreeMap::new();
        let mut oldest_by_policy = BTreeMap::<(String, u64, &'static str), (i64, u64)>::new();
        for ((route, version), thresholds) in &self.config.thresholds {
            for (state, threshold) in [
                ("detected", thresholds.detected),
                ("confirmed", thresholds.confirmed),
                ("cleared", thresholds.cleared),
                ("credited", thresholds.credited),
            ] {
                oldest_by_policy.insert((route.clone(), *version, state), (0, threshold));
            }
        }
        let mut alerts = self.alerts.lock().await;
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
            observed_states.insert(row.id, state);
            let Some(threshold) = self.config.threshold(route, version, state) else {
                continue;
            };
            let age_seconds = now.signed_duration_since(row.entered_at).num_seconds();
            let Ok(threshold_seconds) = i64::try_from(threshold) else {
                continue;
            };
            let state_label = state_code(state);
            oldest_by_policy
                .entry((route.to_owned(), version, state_label))
                .and_modify(|(oldest, _)| *oldest = (*oldest).max(age_seconds))
                .or_insert((age_seconds, threshold));
            let reminder_due = alerts.get(&row.id).is_none_or(|alert| {
                alert.state != state
                    || alert_now.duration_since(alert.last_alerted_at) >= self.reminder_interval
            });
            if age_seconds > threshold_seconds && reminder_due {
                tracing::warn!(
                    tags.alert = "TopupDepositStateAgeExceeded",
                    tags.route = route,
                    tags.state = state_label,
                    deposit_id = %row.id,
                    route,
                    route_version = version,
                    state = ?state,
                    age_seconds,
                    threshold_seconds,
                    "deposit has exceeded its state-age threshold"
                );
                alert_count = alert_count.saturating_add(1);
                alerts.insert(
                    row.id,
                    AlertRecord {
                        state,
                        last_alerted_at: alert_now,
                    },
                );
            }
        }
        for ((route, version, state), (age, threshold)) in oldest_by_policy {
            let version = version.to_string();
            metrics::gauge!(
                "topup_deposit_state_age_seconds",
                "state" => state,
                "route" => route.clone(),
                "route_version" => version.clone(),
                "producer_enabled" => "true",
            )
            .set(age.max(0) as f64);
            metrics::gauge!(
                "topup_deposit_state_age_policy_seconds",
                "state" => state,
                "route" => route,
                "route_version" => version,
                "producer_enabled" => "true",
            )
            .set(threshold as f64);
        }
        alerts.retain(|id, alert| observed_states.get(id) == Some(&alert.state));
        Ok(alert_count)
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

struct AlertRecord {
    state: DepositState,
    last_alerted_at: Instant,
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
