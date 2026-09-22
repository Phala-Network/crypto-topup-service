//! JSON report types emitted by the conformance runner.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

use crate::chain::Manifest;

/// Stable report schema version.
pub const REPORT_VERSION: u32 = 2;

/// Overall conformance run report.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Report schema version.
    pub version: u32,
    /// Endpoint exercised by the suite.
    pub settlement_url: String,
    /// Chain fixture the run used.
    pub manifest: Manifest,
    /// UTC start time.
    pub started_at: DateTime<Utc>,
    /// UTC completion time.
    pub finished_at: DateTime<Utc>,
    /// True only when every case passed; any `fail` or `incomplete` case makes this false.
    pub passed: bool,
    /// Aggregate result counts.
    pub summary: Summary,
    /// Individual test results and bounded evidence.
    pub tests: Vec<TestResult>,
}

/// Aggregate result counts.
#[derive(Debug, Serialize)]
pub struct Summary {
    /// Passed cases.
    pub passed: usize,
    /// Failed cases.
    pub failed: usize,
    /// Cases whose required observation was unavailable.
    pub incomplete: usize,
    /// Warnings across all cases; they never affect `passed`.
    pub warnings: usize,
}

/// Result of one independently named conformance case.
#[derive(Debug, Serialize)]
pub struct TestResult {
    /// Stable machine-readable case identifier.
    pub id: String,
    /// Human-readable assertion name.
    pub name: String,
    /// Architecture section 11 product obligation, or `null` for protocol cases.
    pub obligation: Option<u8>,
    /// Pass, fail, or incomplete.
    pub status: TestStatus,
    /// Sanitized evidence suitable for CI artifacts.
    pub evidence: Value,
    /// Behavior the documentation forbids but this case does not enforce.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Case outcome classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    /// The endpoint met the assertion.
    Pass,
    /// The endpoint violated the assertion or could not be exercised.
    Fail,
    /// A required observation (ledger hook or restart) was unavailable; never a pass.
    Incomplete,
}

impl Report {
    /// Builds aggregate counts from completed cases.
    #[must_use]
    pub fn complete(
        settlement_url: String,
        manifest: Manifest,
        started_at: DateTime<Utc>,
        tests: Vec<TestResult>,
    ) -> Self {
        let count = |status| tests.iter().filter(|test| test.status == status).count();
        let summary = Summary {
            passed: count(TestStatus::Pass),
            failed: count(TestStatus::Fail),
            incomplete: count(TestStatus::Incomplete),
            warnings: tests.iter().map(|test| test.warnings.len()).sum(),
        };
        Self {
            version: REPORT_VERSION,
            settlement_url,
            manifest,
            started_at,
            finished_at: Utc::now(),
            passed: summary.failed == 0 && summary.incomplete == 0,
            summary,
            tests,
        }
    }
}
