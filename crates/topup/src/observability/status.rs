//! Latest reconciliation round, for the admin daily report.
//!
//! Production CVMs have no logs and Sentry events share a quota, so without this an operator
//! sees that a check failed but not why. The state is per process: a restarted service reports
//! nothing until its first reconciliation round.

use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};

/// Latest reconciliation round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationStatus {
    /// When the round finished.
    pub at: DateTime<Utc>,
    /// Check code and redacted error of every check that could not complete.
    pub failed_checks: Vec<(String, String)>,
}

static RECONCILIATION: Mutex<Option<ReconciliationStatus>> = Mutex::new(None);

/// Records a finished reconciliation round.
pub fn record_reconciliation(failed_checks: Vec<(String, String)>) {
    *RECONCILIATION
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = Some(ReconciliationStatus {
        at: Utc::now(),
        failed_checks,
    });
}

/// Returns the latest reconciliation round in this process.
#[must_use]
pub fn reconciliation() -> Option<ReconciliationStatus> {
    RECONCILIATION
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}
