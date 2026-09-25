//! Latest outcomes of scheduled flush planning and reconciliation, for the admin daily report.
//!
//! Production CVMs have no logs and Sentry events share a quota, so without this an operator
//! sees that forwarders stay unswept but not why. The state is per process: a restarted service
//! reports nothing until its first planning run or reconciliation round.

use std::collections::BTreeMap;
use std::sync::{Mutex, PoisonError};

use chrono::{DateTime, Utc};

/// Result of one scheduled flush planning run of a route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlushPlanningOutcome {
    /// A plan was written or an unsigned plan was rebound, and its send was attempted.
    Planned,
    /// Planning completed without a plan: nothing met the flush policy, a flush is in flight, or
    /// reconciliation froze the chain.
    Idle,
    /// The operator was not known to hold `OPERATOR_ROLE` (missing, or not yet confirmed because
    /// the role check failed), so planning did not run.
    OperatorNotAuthorized,
    /// Planning failed.
    Failed,
    /// Planning succeeded, but sending or maintaining a flush afterwards failed.
    SendFailed,
}

impl FlushPlanningOutcome {
    /// Stable report code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Idle => "idle",
            Self::OperatorNotAuthorized => "operator_not_authorized",
            Self::Failed => "failed",
            Self::SendFailed => "send_failed",
        }
    }
}

/// Latest scheduled flush planning run of one route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlushPlanningStatus {
    /// When the run finished.
    pub at: DateTime<Utc>,
    /// What the run did.
    pub outcome: FlushPlanningOutcome,
    /// The redacted error of a failed run.
    pub error: Option<String>,
}

/// Latest reconciliation round.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReconciliationStatus {
    /// When the round finished.
    pub at: DateTime<Utc>,
    /// Check code and redacted error of every check that could not complete.
    pub failed_checks: Vec<(String, String)>,
}

static FLUSH_PLANNING: Mutex<BTreeMap<String, FlushPlanningStatus>> = Mutex::new(BTreeMap::new());
static RECONCILIATION: Mutex<Option<ReconciliationStatus>> = Mutex::new(None);

/// Records the outcome of a route's scheduled flush planning run.
pub fn record_flush_planning(route: &str, outcome: FlushPlanningOutcome, error: Option<String>) {
    let status = FlushPlanningStatus {
        at: Utc::now(),
        outcome,
        error,
    };
    FLUSH_PLANNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(route.to_owned(), status);
}

/// Returns the latest scheduled flush planning run of a route in this process.
#[must_use]
pub fn flush_planning(route: &str) -> Option<FlushPlanningStatus> {
    FLUSH_PLANNING
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .get(route)
        .cloned()
}

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
