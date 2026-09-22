use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use metrics::{counter, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use sqlx::{PgPool, Row};
use tokio::time::{MissedTickBehavior, interval};
use tokio_util::sync::CancellationToken;

static PROMETHEUS: OnceLock<PrometheusHandle> = OnceLock::new();
const DEFAULT_BACKUP_TIMESTAMP_FILE: &str = "/run/topup-observability/last-backup-unix-seconds";
const COLLECTION_INTERVAL: Duration = Duration::from_secs(15);
const DEPOSIT_STATES: [&str; 6] = [
    "detected",
    "confirmed",
    "cleared",
    "credited",
    "swept",
    "rejected",
];

/// Failure to install the process-global Prometheus recorder.
#[derive(Debug)]
pub struct InitError(String);

impl Display for InitError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "failed to initialize Prometheus metrics: {}",
            self.0
        )
    }
}

impl Error for InitError {}

/// Installs the Prometheus recorder and registers the stable metric contract.
pub fn init() -> Result<(), InitError> {
    if PROMETHEUS.get().is_some() {
        return Ok(());
    }
    let recorder = PrometheusBuilder::new().build_recorder();
    let handle = recorder.handle();
    metrics::set_global_recorder(recorder).map_err(|error| InitError(error.to_string()))?;
    register_metrics();
    PROMETHEUS
        .set(handle)
        .map_err(|_| InitError("recorder handle was already installed".to_owned()))
}

/// Registers all §16 names, including producers owned by later work packages.
pub fn register_metrics() {
    describe_gauge!(
        "topup_scanner_lag_blocks",
        "Finalized blocks not yet committed by the scanner"
    );
    describe_gauge!(
        "topup_scanner_lag_seconds",
        "Seconds since the scanner last completed a successful pass"
    );
    describe_gauge!("topup_deposits", "Current deposits grouped by state");
    describe_gauge!(
        "topup_deposit_state_age_seconds",
        "Oldest current deposit age grouped by state and route policy"
    );
    describe_gauge!(
        "topup_deposit_state_age_policy_seconds",
        "Configured maximum age for a deposit state"
    );
    describe_counter!(
        "topup_provider_disagreements_total",
        "Independent provider disagreements"
    );
    describe_gauge!(
        "topup_price_deviation_basis_points",
        "Primary/check price deviation in basis points"
    );
    describe_gauge!(
        "topup_fx_deviation_basis_points",
        "Observed FX deviation in basis points"
    );
    describe_counter!(
        "topup_settlement_outcomes_total",
        "Settlement outcomes grouped by type"
    );
    describe_gauge!("topup_outbox_backlog", "Pending outbox events");
    describe_gauge!(
        "topup_outbox_oldest_age_seconds",
        "Age of the oldest pending outbox event"
    );
    describe_gauge!(
        "topup_unflushed_balance_atomic",
        "Unflushed asset balance grouped by route"
    );
    describe_gauge!(
        "topup_operator_gas_balance_wei",
        "Operator native gas balance in wei"
    );
    describe_gauge!(
        "topup_open_lock_exposure_minor",
        "Open rate-lock exposure in destination minor units"
    );
    describe_gauge!(
        "topup_open_lock_exposure_cap_minor",
        "Configured open rate-lock exposure cap in destination minor units"
    );
    describe_gauge!(
        "topup_backup_age_seconds",
        "Age of the last successful WAL-G base backup marker"
    );
    describe_counter!(
        "topup_reconciliation_mismatches_total",
        "Reconciliation mismatches grouped by check"
    );
    describe_gauge!(
        "topup_loop_heartbeat_unixtime_seconds",
        "Unix time of the most recent loop iteration"
    );
    describe_counter!(
        "topup_unsupported_inflows_total",
        "Finalized unsupported-asset inflows observed by the scanner"
    );

    // C4/C6/C7/C8 consume these stable names when their producers merge. Their pending-labelled
    // zero series keep dashboards and alert expressions reviewable without inventing fake data.
    gauge!("topup_scanner_lag_blocks", "chain" => "pending").set(0);
    gauge!("topup_scanner_lag_seconds", "chain" => "pending").set(0);
    for state in DEPOSIT_STATES {
        gauge!("topup_deposits", "state" => state).set(0);
    }
    gauge!("topup_deposit_state_age_seconds", "state" => "pending", "route" => "pending", "route_version" => "0").set(0);
    gauge!("topup_deposit_state_age_policy_seconds", "state" => "pending", "route" => "pending", "route_version" => "0").set(0);
    counter!("topup_provider_disagreements_total", "chain" => "pending").absolute(0);
    gauge!("topup_price_deviation_basis_points", "route" => "pending").set(0);
    gauge!("topup_fx_deviation_basis_points", "route" => "pending").set(0);
    counter!("topup_settlement_outcomes_total", "outcome" => "pending").absolute(0);
    gauge!("topup_outbox_backlog").set(0);
    gauge!("topup_outbox_oldest_age_seconds").set(0);
    gauge!("topup_unflushed_balance_atomic", "route" => "pending").set(0);
    gauge!("topup_operator_gas_balance_wei", "chain" => "pending").set(0);
    gauge!("topup_open_lock_exposure_minor", "scope" => "pending", "id" => "pending").set(0);
    gauge!("topup_open_lock_exposure_cap_minor", "scope" => "pending", "id" => "pending").set(0);
    gauge!("topup_backup_age_seconds").set(f64::INFINITY);
    counter!("topup_reconciliation_mismatches_total", "check" => "pending").absolute(0);
    counter!("topup_unsupported_inflows_total", "chain" => "pending").absolute(0);
    for loop_name in ["pump", "scanner", "outbox", "flusher", "reconciler"] {
        gauge!("topup_loop_heartbeat_unixtime_seconds", "loop" => loop_name).set(0);
    }
}

/// Renders the process metrics in Prometheus text exposition format.
pub async fn metrics_response() -> Response {
    let Some(handle) = PROMETHEUS.get() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let mut response = handle.render().into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );
    response
}

/// Records that a named service loop completed or attempted one iteration.
pub fn heartbeat(loop_name: &'static str) {
    gauge!("topup_loop_heartbeat_unixtime_seconds", "loop" => loop_name)
        .set(metric_value(unix_now()));
}

/// Updates scanner lag after one pass.
pub fn record_scanner_lag(chain: u64, finalized: u64, cursor: u64, seconds: u64) {
    let chain = chain.to_string();
    gauge!("topup_scanner_lag_blocks", "chain" => chain.clone())
        .set(metric_value(finalized.saturating_sub(cursor)));
    gauge!("topup_scanner_lag_seconds", "chain" => chain).set(metric_value(seconds));
}

/// Periodically refreshes metrics sourced from PostgreSQL and the backup marker file.
pub async fn collect_database_metrics(pool: PgPool, cancellation: CancellationToken) {
    let marker = std::env::var_os("TOPUP_BACKUP_TIMESTAMP_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BACKUP_TIMESTAMP_FILE));
    let mut ticker = interval(COLLECTION_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = cancellation.cancelled() => return,
            _ = ticker.tick() => {
                if let Err(error) = collect_once(&pool, &marker).await {
                    tracing::error!(%error, "observability metric collection failed");
                }
            }
        }
    }
}

async fn collect_once(pool: &PgPool, marker: &Path) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("SELECT state, count(*)::bigint AS count FROM deposits GROUP BY state")
        .fetch_all(pool)
        .await?;
    for state in DEPOSIT_STATES {
        gauge!("topup_deposits", "state" => state).set(0);
    }
    for row in rows {
        let state: String = row.try_get("state")?;
        let count: i64 = row.try_get("count")?;
        gauge!("topup_deposits", "state" => state)
            .set(metric_value(u64::try_from(count).unwrap_or_default()));
    }

    let unflushed = sqlx::query(
        r#"
        SELECT route,
               COALESCE(sum(amount_atomic) FILTER (WHERE flush_id IS NULL), 0)::text AS amount
        FROM deposits
        WHERE route IS NOT NULL
        GROUP BY route
        "#,
    )
    .fetch_all(pool)
    .await?;
    for row in unflushed {
        let route: String = row.try_get("route")?;
        let amount: String = row.try_get("amount")?;
        let amount = amount.parse::<f64>().unwrap_or(f64::INFINITY);
        gauge!("topup_unflushed_balance_atomic", "route" => route).set(amount);
    }

    let outbox = sqlx::query(
        r#"
        SELECT count(*)::bigint AS count,
               COALESCE(EXTRACT(EPOCH FROM now() - min(created_at)), 0)::double precision AS age
        FROM outbox
        WHERE delivered_at IS NULL
        "#,
    )
    .fetch_one(pool)
    .await?;
    let backlog: i64 = outbox.try_get("count")?;
    let oldest_age: f64 = outbox.try_get("age")?;
    gauge!("topup_outbox_backlog").set(metric_value(u64::try_from(backlog).unwrap_or_default()));
    gauge!("topup_outbox_oldest_age_seconds").set(oldest_age.max(0.0));
    gauge!("topup_backup_age_seconds").set(backup_age(marker));
    Ok(())
}

fn backup_age(path: &Path) -> f64 {
    let timestamp = std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok());
    match timestamp {
        Some(timestamp) => unix_now().saturating_sub(timestamp) as f64,
        None => f64::INFINITY,
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn metric_value(value: u64) -> f64 {
    value as f64
}

#[cfg(test)]
mod tests {
    use metrics_exporter_prometheus::PrometheusBuilder;

    use super::register_metrics;

    #[test]
    fn registered_contract_contains_every_metric_name() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, register_metrics);
        let rendered = handle.render();
        for name in [
            "topup_scanner_lag_blocks",
            "topup_scanner_lag_seconds",
            "topup_deposits",
            "topup_deposit_state_age_seconds",
            "topup_provider_disagreements_total",
            "topup_price_deviation_basis_points",
            "topup_fx_deviation_basis_points",
            "topup_settlement_outcomes_total",
            "topup_outbox_backlog",
            "topup_outbox_oldest_age_seconds",
            "topup_unflushed_balance_atomic",
            "topup_operator_gas_balance_wei",
            "topup_open_lock_exposure_minor",
            "topup_backup_age_seconds",
            "topup_reconciliation_mismatches_total",
            "topup_loop_heartbeat_unixtime_seconds",
        ] {
            assert!(rendered.contains(name), "missing {name}\n{rendered}");
        }
    }
}
