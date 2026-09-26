//! Tracing conventions, secret redaction, Prometheus metrics, and optional Sentry reporting.

mod logging;
mod metrics;
mod redaction;
mod reporting;
mod request;
mod spans;
mod status;

pub use logging::log_subscriber;
pub use metrics::{
    InitError, LockExposureCaps, clear_execution_deadline, collect_backup_metrics,
    collect_database_metrics, execution_deadline, heartbeat, init, metrics_response,
    metrics_router, progress, record_flush_send_paused, record_lock_expiry_failure,
    record_scanner_lag, record_scanner_success, register_loop, register_metrics, register_scanner,
    waiting,
};
pub use redaction::{Redacted, RedactedTransportError};
pub use reporting::{CronMonitor, ReportingError, init_reporting};
pub use request::request_context;
pub use spans::{deposit_step_span, flush_action_span, outbox_delivery_span, scanner_window_span};
pub use status::{
    FlushPlanningOutcome, FlushPlanningStatus, ReconciliationStatus, flush_planning,
    reconciliation, record_flush_planning, record_reconciliation,
};
