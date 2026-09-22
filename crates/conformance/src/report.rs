//! JSON report types emitted by the conformance runner.

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;

/// Stable report schema version.
pub const REPORT_VERSION: u32 = 1;

/// Overall conformance run report.
#[derive(Debug, Serialize)]
pub struct Report {
    /// Report schema version.
    pub version: u32,
    /// Endpoint exercised by the suite.
    pub settlement_url: String,
    /// UTC start time.
    pub started_at: DateTime<Utc>,
    /// UTC completion time.
    pub finished_at: DateTime<Utc>,
    /// True only when no test failed.
    pub passed: bool,
    /// Aggregate result counts.
    pub summary: Summary,
    /// Individual test results and bounded evidence.
    pub tests: Vec<TestResult>,
}

/// Aggregate result counts.
#[derive(Debug, Serialize)]
pub struct Summary {
    /// Passed tests.
    pub passed: usize,
    /// Failed tests.
    pub failed: usize,
    /// Explicitly skipped optional tests.
    pub skipped: usize,
}

/// Result of one independently named conformance assertion.
#[derive(Debug, Serialize)]
pub struct TestResult {
    /// Stable machine-readable test identifier.
    pub id: String,
    /// Human-readable assertion name.
    pub name: String,
    /// Pass, fail, or skip.
    pub status: TestStatus,
    /// Sanitized evidence suitable for CI artifacts.
    pub evidence: Value,
}

/// Test outcome classification.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TestStatus {
    /// The endpoint met the assertion.
    Pass,
    /// The endpoint violated the assertion or could not be exercised.
    Fail,
    /// An optional dependency was not configured.
    Skip,
}

impl Report {
    /// Builds aggregate counts from completed tests.
    #[must_use]
    pub fn complete(
        settlement_url: String,
        started_at: DateTime<Utc>,
        tests: Vec<TestResult>,
    ) -> Self {
        let passed = tests
            .iter()
            .filter(|test| test.status == TestStatus::Pass)
            .count();
        let failed = tests
            .iter()
            .filter(|test| test.status == TestStatus::Fail)
            .count();
        let skipped = tests
            .iter()
            .filter(|test| test.status == TestStatus::Skip)
            .count();
        Self {
            version: REPORT_VERSION,
            settlement_url,
            started_at,
            finished_at: Utc::now(),
            passed: failed == 0,
            summary: Summary {
                passed,
                failed,
                skipped,
            },
            tests,
        }
    }
}
