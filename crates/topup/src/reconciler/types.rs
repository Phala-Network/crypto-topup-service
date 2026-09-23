use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use chrono::Utc;
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// Stable reconciliation check identifier used by findings, metrics, and alerts.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckName {
    /// A finalized transfer was absent from the deposit ledger.
    MissingDeposit,
    /// A sent settlement needed authoritative product lookup.
    SentSettlement,
    /// Stored credit disagreed with deterministic recomputation.
    CreditRecomputation,
    /// A deposit was not linked to a later confirmed flush.
    MissingFlushLink,
    /// Custody balances or persisted flush totals disagreed with chain state.
    CustodyBalance,
    /// The factory-derived address disagreed with stored address data.
    AddressDerivation,
    /// Post-restore product truth was absent or could not be adopted.
    PostRestoreSettlement,
    /// A rate-lock exposure counter disagreed with its open reserved locks.
    LockExposure,
}

impl CheckName {
    /// Every check, including the post-restore gate, in metric registration order.
    pub const ALL: [Self; 8] = [
        Self::MissingDeposit,
        Self::SentSettlement,
        Self::CreditRecomputation,
        Self::MissingFlushLink,
        Self::CustodyBalance,
        Self::AddressDerivation,
        Self::PostRestoreSettlement,
        Self::LockExposure,
    ];

    /// Returns the stable metric label value.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingDeposit => "missing_deposit",
            Self::SentSettlement => "sent_settlement",
            Self::CreditRecomputation => "credit_recomputation",
            Self::MissingFlushLink => "missing_flush_link",
            Self::CustodyBalance => "custody_balance",
            Self::AddressDerivation => "address_derivation",
            Self::PostRestoreSettlement => "post_restore_settlement",
            Self::LockExposure => "lock_exposure",
        }
    }
}

/// One typed reconciliation observation and its repair outcome.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Finding {
    /// Stable identifier derived from the complete finding fingerprint.
    pub id: Uuid,
    /// Reconciliation check which produced the finding.
    pub check: CheckName,
    /// Stable subject identifiers such as chain, address, or deposit ids.
    pub subjects: BTreeMap<String, String>,
    /// Expected state encoded without lossy numeric conversions.
    pub expected: Value,
    /// Observed state encoded without lossy numeric conversions.
    pub observed: Value,
    /// Whether the exact safe repair allowed by the specification was applied.
    pub repair_applied: bool,
    /// Whether post-restore reconciliation must prevent service resumption.
    pub incomplete: bool,
    pub(crate) fingerprint: String,
}

impl Finding {
    pub(crate) fn new(
        check: CheckName,
        subjects: BTreeMap<String, String>,
        expected: Value,
        observed: Value,
        repair_applied: bool,
        incomplete: bool,
    ) -> Result<Self, serde_json::Error> {
        let material = serde_json::to_vec(&(
            check,
            &subjects,
            &expected,
            &observed,
            repair_applied,
            incomplete,
        ))?;
        let fingerprint = hex::encode(Sha256::digest(material));
        let id = Uuid::new_v5(&Uuid::NAMESPACE_OID, fingerprint.as_bytes());
        Ok(Self {
            id,
            check,
            subjects,
            expected,
            observed,
            repair_applied,
            incomplete,
            fingerprint,
        })
    }
}

/// Aggregate result of one reconciliation pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ReconciliationReport {
    /// Findings observed during this pass, including already-deduplicated durable findings.
    pub findings: Vec<Finding>,
    /// Checks which could not complete; every other check still ran.
    pub failed_checks: Vec<CheckName>,
    /// Whether a post-restore caller must keep the service stopped.
    ///
    /// Only the post-restore product lookups set this flag; alert-only findings never do.
    pub incomplete: bool,
}

impl ReconciliationReport {
    /// Returns whether every check completed.
    #[must_use]
    pub fn succeeded(&self) -> bool {
        self.failed_checks.is_empty()
    }
}

/// In-process counters mirrored to the §16 reconciliation metric and loop progress gauge.
#[derive(Debug, Default)]
pub struct ReconciliationMetrics {
    missing_deposit: AtomicU64,
    sent_settlement: AtomicU64,
    credit_recomputation: AtomicU64,
    missing_flush_link: AtomicU64,
    custody_balance: AtomicU64,
    address_derivation: AtomicU64,
    post_restore_settlement: AtomicU64,
    lock_exposure: AtomicU64,
    last_heartbeat_unix: AtomicI64,
}

impl ReconciliationMetrics {
    /// Prometheus metric name reserved by the architecture.
    pub const MISMATCH_METRIC: &'static str = "topup_reconciliation_mismatches_total";

    pub(crate) fn record_mismatch(&self, check: CheckName) {
        let counter = match check {
            CheckName::MissingDeposit => &self.missing_deposit,
            CheckName::SentSettlement => &self.sent_settlement,
            CheckName::CreditRecomputation => &self.credit_recomputation,
            CheckName::MissingFlushLink => &self.missing_flush_link,
            CheckName::CustodyBalance => &self.custody_balance,
            CheckName::AddressDerivation => &self.address_derivation,
            CheckName::PostRestoreSettlement => &self.post_restore_settlement,
            CheckName::LockExposure => &self.lock_exposure,
        };
        counter.fetch_add(1, Ordering::Relaxed);
        metrics::counter!(Self::MISMATCH_METRIC, "check" => check.code(), "producer_enabled" => "true")
            .increment(1);
    }

    pub(crate) fn heartbeat(&self) {
        self.last_heartbeat_unix
            .store(Utc::now().timestamp(), Ordering::Relaxed);
        crate::observability::progress(super::LOOP_NAME, super::LOOP_INSTANCE);
    }

    /// Returns the mismatch count for one check in this process.
    #[must_use]
    pub fn mismatch_count(&self, check: CheckName) -> u64 {
        let counter = match check {
            CheckName::MissingDeposit => &self.missing_deposit,
            CheckName::SentSettlement => &self.sent_settlement,
            CheckName::CreditRecomputation => &self.credit_recomputation,
            CheckName::MissingFlushLink => &self.missing_flush_link,
            CheckName::CustodyBalance => &self.custody_balance,
            CheckName::AddressDerivation => &self.address_derivation,
            CheckName::PostRestoreSettlement => &self.post_restore_settlement,
            CheckName::LockExposure => &self.lock_exposure,
        };
        counter.load(Ordering::Relaxed)
    }

    /// Returns the end of the last round in which every check completed, as a Unix timestamp.
    #[must_use]
    pub fn last_heartbeat_unix(&self) -> i64 {
        self.last_heartbeat_unix.load(Ordering::Relaxed)
    }
}
