//! Tracing conventions, secret redaction, and optional Sentry reporting.

mod backup;
mod logging;
mod redaction;
mod reporting;
mod request;
mod spans;
mod status;

pub use backup::monitor_backup;
pub use logging::log_subscriber;
pub use redaction::{Redacted, RedactedTransportError};
pub use reporting::{CronMonitor, ReportingError, init_reporting};
pub use request::request_context;
pub use spans::{deposit_step_span, flush_action_span, outbox_delivery_span, scanner_window_span};
pub use status::{
    FlushPlanningOutcome, FlushPlanningStatus, ReconciliationStatus, flush_planning,
    reconciliation, record_flush_planning, record_reconciliation,
};
