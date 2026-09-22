use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use metrics::{counter, describe_counter, describe_gauge, gauge};
use metrics_exporter_prometheus::{PrometheusBuilder, PrometheusHandle};
use sqlx::{PgPool, Row};
use tokio::time::sleep;
use tokio_util::sync::CancellationToken;
use topup_core::route::RouteFile;

use crate::reconciler::CheckName;

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
    describe_gauge!(
        "topup_scanner_last_success_unixtime_seconds",
        "Unix time of the scanner's most recent successful pass"
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
    describe_gauge!(
        "topup_backup_last_success_unixtime_seconds",
        "Unix timestamp stored by the most recent successful WAL-G operation"
    );
    describe_counter!(
        "topup_reconciliation_mismatches_total",
        "Reconciliation mismatches grouped by check"
    );
    describe_gauge!(
        "topup_loop_heartbeat_unixtime_seconds",
        "Unix time of the most recent loop iteration"
    );
    describe_gauge!(
        "topup_loop_progress_unixtime_seconds",
        "Unix time when a loop most recently completed useful work"
    );
    describe_gauge!(
        "topup_loop_wait_until_unixtime_seconds",
        "Unix time until which a loop is intentionally waiting"
    );
    describe_gauge!(
        "topup_loop_deadline_unixtime_seconds",
        "Unix time by which the current loop iteration is expected to complete"
    );
    describe_gauge!(
        "topup_loop_expected",
        "Whether a loop instance is expected to emit heartbeats"
    );
    describe_gauge!(
        "topup_flush_send_paused",
        "Whether the next planned flush on a chain is held by a flush pause scope"
    );
    describe_counter!(
        "topup_unsupported_inflows_total",
        "Finalized unsupported-asset inflows observed by the scanner"
    );

    // Producers owned by later work packages consume these stable names when they merge. Their
    // pending-labelled zero series keep dashboards and alerts reviewable without fake data.
    gauge!("topup_scanner_lag_blocks", "chain" => "pending", "producer_enabled" => "false").set(0);
    gauge!("topup_scanner_lag_seconds", "chain" => "pending", "producer_enabled" => "false").set(0);
    gauge!("topup_scanner_last_success_unixtime_seconds", "chain" => "pending", "producer_enabled" => "false").set(0);
    for state in DEPOSIT_STATES {
        gauge!("topup_deposits", "state" => state, "producer_enabled" => "true").set(0);
    }
    gauge!("topup_deposit_state_age_seconds", "state" => "pending", "route" => "pending", "route_version" => "0", "producer_enabled" => "false").set(0);
    gauge!("topup_deposit_state_age_policy_seconds", "state" => "pending", "route" => "pending", "route_version" => "0", "producer_enabled" => "false").set(0);
    counter!("topup_provider_disagreements_total", "chain" => "pending", "producer_enabled" => "false").absolute(0);
    gauge!("topup_price_deviation_basis_points", "route" => "pending", "producer_enabled" => "false").set(0);
    gauge!("topup_fx_deviation_basis_points", "route" => "pending", "producer_enabled" => "false")
        .set(0);
    counter!("topup_settlement_outcomes_total", "outcome" => "pending", "producer_enabled" => "false").absolute(0);
    gauge!("topup_outbox_backlog", "producer_enabled" => "true").set(0);
    gauge!("topup_outbox_oldest_age_seconds", "producer_enabled" => "true").set(0);
    gauge!("topup_unflushed_balance_atomic", "route" => "pending", "producer_enabled" => "false")
        .set(0);
    gauge!("topup_operator_gas_balance_wei", "chain" => "pending", "producer_enabled" => "false")
        .set(0);
    gauge!("topup_open_lock_exposure_minor", "scope" => "pending", "id" => "pending", "producer_enabled" => "false").set(0);
    gauge!("topup_open_lock_exposure_cap_minor", "scope" => "pending", "id" => "pending", "producer_enabled" => "false").set(0);
    gauge!("topup_backup_age_seconds", "producer_enabled" => "false").set(0);
    gauge!("topup_backup_last_success_unixtime_seconds", "producer_enabled" => "true").set(0);
    // Zero-initialized so `increase()` observes the first mismatch of every check.
    for check in CheckName::ALL {
        counter!("topup_reconciliation_mismatches_total", "check" => check.code(), "producer_enabled" => "true").absolute(0);
    }
    counter!("topup_unsupported_inflows_total", "chain" => "pending", "producer_enabled" => "false").absolute(0);
    for loop_name in [
        "pump",
        "scanner",
        "outbox",
        "flusher",
        "reconciler",
        "lock_expiry",
    ] {
        gauge!("topup_loop_expected", "loop" => loop_name, "loop_instance" => "pending", "producer_enabled" => "false").set(0);
        gauge!("topup_loop_heartbeat_unixtime_seconds", "loop" => loop_name, "loop_instance" => "pending", "producer_enabled" => "false").set(0);
        gauge!("topup_loop_progress_unixtime_seconds", "loop" => loop_name, "loop_instance" => "pending", "producer_enabled" => "false").set(0);
        gauge!("topup_loop_wait_until_unixtime_seconds", "loop" => loop_name, "loop_instance" => "pending", "producer_enabled" => "false").set(0);
        gauge!("topup_loop_deadline_unixtime_seconds", "loop" => loop_name, "loop_instance" => "pending", "producer_enabled" => "false").set(0);
    }
    gauge!("topup_loop_expected", "loop" => "metrics", "loop_instance" => "pending", "producer_enabled" => "false").set(0);
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

/// Builds the unauthenticated router served only on the monitoring listener.
pub fn metrics_router() -> Router {
    Router::new().route("/metrics", get(metrics_response))
}

/// Registers one expected loop instance before its task starts.
pub fn register_loop(loop_name: &'static str, loop_instance: impl Into<String>) {
    let loop_instance = loop_instance.into();
    gauge!("topup_loop_expected", "loop" => loop_name, "loop_instance" => loop_instance.clone(), "producer_enabled" => "true").set(1);
    gauge!("topup_loop_wait_until_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.clone(), "producer_enabled" => "true").set(0);
    gauge!("topup_loop_deadline_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance, "producer_enabled" => "true").set(0);
}

/// Records that a named service loop began one iteration.
pub fn heartbeat(loop_name: &'static str, loop_instance: impl Into<String>) {
    let loop_instance = loop_instance.into();
    gauge!("topup_loop_heartbeat_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.clone(), "producer_enabled" => "true")
        .set(metric_value(unix_now()));
    gauge!("topup_loop_wait_until_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance, "producer_enabled" => "true").set(0);
}

/// Records that a loop instance completed useful work.
pub fn progress(loop_name: &'static str, loop_instance: impl Into<String>) {
    gauge!("topup_loop_progress_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.into(), "producer_enabled" => "true")
        .set(metric_value(unix_now()));
}

/// Records an intentional wait so stopped-loop alerts allow the full delay.
pub fn waiting(loop_name: &'static str, loop_instance: impl Into<String>, delay: Duration) {
    let loop_instance = loop_instance.into();
    gauge!("topup_loop_wait_until_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.clone(), "producer_enabled" => "true")
        .set(metric_value(unix_now().saturating_add(delay.as_secs())));
    clear_execution_deadline(loop_name, loop_instance);
}

/// Records the expected completion time of an in-flight loop iteration.
pub fn execution_deadline(
    loop_name: &'static str,
    loop_instance: impl Into<String>,
    duration: Duration,
) {
    gauge!("topup_loop_deadline_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.into(), "producer_enabled" => "true")
        .set(metric_value(unix_now().saturating_add(duration.as_secs())));
}

/// Clears the execution deadline after an iteration completes.
pub fn clear_execution_deadline(loop_name: &'static str, loop_instance: impl Into<String>) {
    gauge!("topup_loop_deadline_unixtime_seconds", "loop" => loop_name, "loop_instance" => loop_instance.into(), "producer_enabled" => "true").set(0);
}

/// Updates scanner lag after one pass.
pub fn record_scanner_lag(chain: u64, finalized: u64, cursor: u64, seconds: u64) {
    let chain = chain.to_string();
    gauge!("topup_scanner_lag_blocks", "chain" => chain.clone(), "producer_enabled" => "true")
        .set(metric_value(finalized.saturating_sub(cursor)));
    gauge!("topup_scanner_lag_seconds", "chain" => chain, "producer_enabled" => "true")
        .set(metric_value(seconds));
}

/// Registers a configured scanner before its first provider request.
pub fn register_scanner(chain: u64) {
    let chain = chain.to_string();
    register_loop("scanner", chain.clone());
    gauge!("topup_scanner_lag_blocks", "chain" => chain.clone(), "producer_enabled" => "true")
        .set(0);
    gauge!("topup_scanner_lag_seconds", "chain" => chain.clone(), "producer_enabled" => "true")
        .set(0);
    gauge!("topup_scanner_last_success_unixtime_seconds", "chain" => chain, "producer_enabled" => "true")
        .set(0);
}

/// Records the timestamp of a successful scanner pass.
pub fn record_scanner_success(chain: u64) {
    gauge!("topup_scanner_last_success_unixtime_seconds", "chain" => chain.to_string(), "producer_enabled" => "true")
        .set(metric_value(unix_now()));
}

/// Rate-lock exposure caps exported beside the observed product and global exposure.
///
/// Account scopes are not exported because their label cardinality grows with accounts.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockExposureCaps {
    global: Option<u64>,
    products: BTreeMap<String, u64>,
}

impl LockExposureCaps {
    /// Uses the tightest cap of every lock-enabled route because routes share one counter.
    #[must_use]
    pub fn from_routes(routes: &[RouteFile]) -> Self {
        let mut caps = Self::default();
        for route in routes.iter().filter(|route| route.rate_lock.enabled) {
            let configured = &route.rate_lock.max_open_minor;
            caps.global = Some(
                caps.global
                    .map_or(configured.global, |cap| cap.min(configured.global)),
            );
            caps.products
                .entry(route.destination.product.clone())
                .and_modify(|cap| *cap = (*cap).min(configured.product))
                .or_insert(configured.product);
        }
        caps
    }

    fn scopes(&self) -> impl Iterator<Item = (&'static str, &str, u64)> {
        self.global
            .map(|cap| ("global", "global", cap))
            .into_iter()
            .chain(
                self.products
                    .iter()
                    .map(|(slug, cap)| ("product", slug.as_str(), *cap)),
            )
    }
}

/// Records whether a chain's next planned flush is held by an operator pause.
pub fn record_flush_send_paused(chain: u64, paused: bool) {
    gauge!("topup_flush_send_paused", "chain" => chain.to_string(), "producer_enabled" => "true")
        .set(if paused { 1.0 } else { 0.0 });
}

/// Periodically refreshes metrics sourced from PostgreSQL.
pub async fn collect_database_metrics(
    pool: PgPool,
    lock_exposure_caps: LockExposureCaps,
    cancellation: CancellationToken,
) {
    let instance = "database";
    register_loop("metrics", instance);
    for (scope, id, cap) in lock_exposure_caps.scopes() {
        gauge!("topup_open_lock_exposure_cap_minor", "scope" => scope, "id" => id.to_owned(), "producer_enabled" => "true")
            .set(metric_value(cap));
        gauge!("topup_open_lock_exposure_minor", "scope" => scope, "id" => id.to_owned(), "producer_enabled" => "true")
            .set(0);
    }
    loop {
        heartbeat("metrics", instance);
        if let Err(error) = collect_database_once(&pool).await {
            tracing::error!(%error, "observability database metric collection failed");
        } else {
            progress("metrics", instance);
        }
        waiting("metrics", instance, COLLECTION_INTERVAL);
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = sleep(COLLECTION_INTERVAL) => {}
        }
    }
}

/// Periodically reads the WAL-G success marker independently of PostgreSQL.
pub async fn collect_backup_metrics(cancellation: CancellationToken) {
    let marker = std::env::var_os("TOPUP_BACKUP_TIMESTAMP_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_BACKUP_TIMESTAMP_FILE));
    let instance = "backup";
    register_loop("metrics", instance);
    loop {
        heartbeat("metrics", instance);
        let timestamp = backup_timestamp(&marker).unwrap_or_default();
        gauge!("topup_backup_last_success_unixtime_seconds", "producer_enabled" => "true")
            .set(metric_value(timestamp));
        if timestamp > 0 {
            progress("metrics", instance);
        }
        waiting("metrics", instance, COLLECTION_INTERVAL);
        tokio::select! {
            () = cancellation.cancelled() => return,
            () = sleep(COLLECTION_INTERVAL) => {}
        }
    }
}

async fn collect_database_once(pool: &PgPool) -> Result<(), sqlx::Error> {
    let rows = sqlx::query("SELECT state, count(*)::bigint AS count FROM deposits GROUP BY state")
        .fetch_all(pool)
        .await?;
    for state in DEPOSIT_STATES {
        gauge!("topup_deposits", "state" => state, "producer_enabled" => "true").set(0);
    }
    for row in rows {
        let state: String = row.try_get("state")?;
        let count: i64 = row.try_get("count")?;
        gauge!("topup_deposits", "state" => state, "producer_enabled" => "true")
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
        gauge!("topup_unflushed_balance_atomic", "route" => route, "producer_enabled" => "true")
            .set(amount);
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
    gauge!("topup_outbox_backlog", "producer_enabled" => "true")
        .set(metric_value(u64::try_from(backlog).unwrap_or_default()));
    gauge!("topup_outbox_oldest_age_seconds", "producer_enabled" => "true")
        .set(oldest_age.max(0.0));

    let exposure = sqlx::query(
        r#"
        SELECT CASE WHEN exposure.scope_key = 'global' THEN 'global' ELSE 'product' END AS scope,
               COALESCE(product.slug, 'global') AS id,
               exposure.open_minor::text AS open_minor
        FROM lock_exposure AS exposure
        LEFT JOIN products AS product ON exposure.scope_key = 'product:' || product.id::text
        WHERE exposure.scope_key = 'global' OR product.id IS NOT NULL
        "#,
    )
    .fetch_all(pool)
    .await?;
    for row in exposure {
        let scope: String = row.try_get("scope")?;
        let id: String = row.try_get("id")?;
        let open: String = row.try_get("open_minor")?;
        let open = open.parse::<f64>().unwrap_or(f64::INFINITY);
        gauge!("topup_open_lock_exposure_minor", "scope" => scope, "id" => id, "producer_enabled" => "true")
            .set(open);
    }
    Ok(())
}

fn backup_timestamp(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
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
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use metrics_exporter_prometheus::PrometheusBuilder;
    use tower::ServiceExt as _;

    use super::{init, metrics_router, register_metrics};

    #[test]
    fn registered_contract_contains_every_metric_name() {
        let recorder = PrometheusBuilder::new().build_recorder();
        let handle = recorder.handle();
        metrics::with_local_recorder(&recorder, register_metrics);
        let rendered = handle.render();
        for name in [
            "topup_scanner_lag_blocks",
            "topup_scanner_lag_seconds",
            "topup_scanner_last_success_unixtime_seconds",
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
            "topup_backup_last_success_unixtime_seconds",
            "topup_reconciliation_mismatches_total",
            "topup_loop_heartbeat_unixtime_seconds",
            "topup_loop_progress_unixtime_seconds",
            "topup_loop_wait_until_unixtime_seconds",
            "topup_loop_deadline_unixtime_seconds",
            "topup_loop_expected",
        ] {
            assert!(rendered.contains(name), "missing {name}\n{rendered}");
        }
    }

    #[tokio::test]
    async fn monitoring_router_exposes_registered_metric_names() {
        init().expect("metrics recorder installs");
        let response = metrics_router()
            .oneshot(
                Request::builder()
                    .uri("/metrics")
                    .body(Body::empty())
                    .expect("request builds"),
            )
            .await
            .expect("metrics request succeeds");
        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("metrics body reads");
        let body = String::from_utf8(body.to_vec()).expect("metrics are UTF-8");
        assert!(body.contains("topup_scanner_lag_blocks"));
        assert!(body.contains("topup_reconciliation_mismatches_total"));
        assert!(body.contains("topup_loop_heartbeat_unixtime_seconds"));
    }
}
