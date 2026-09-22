//! Tracing conventions, secret redaction, and Prometheus metrics.

mod metrics;
mod redaction;
mod request;
mod spans;

pub use metrics::{
    InitError, collect_backup_metrics, collect_database_metrics, heartbeat, init, metrics_response,
    metrics_router, progress, record_scanner_lag, record_scanner_success, register_loop,
    register_metrics, register_scanner, waiting,
};
pub use redaction::{Redacted, RedactedRequestError};
pub use request::request_context;
pub use spans::{deposit_step_span, flush_action_span, outbox_delivery_span, scanner_window_span};
