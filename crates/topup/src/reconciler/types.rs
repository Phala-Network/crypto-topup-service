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
}

impl CheckName {
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
    /// Whether a post-restore caller must keep the service stopped.
    pub incomplete: bool,
}

/// In-process counters reserved under the §16 reconciliation metric name.
#[derive(Debug, Default)]
pub struct ReconciliationMetrics {
    missing_deposit: AtomicU64,
    sent_settlement: AtomicU64,
    credit_recomputation: AtomicU64,
    missing_flush_link: AtomicU64,
    custody_balance: AtomicU64,
    address_derivation: AtomicU64,
    post_restore_settlement: AtomicU64,
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
        };
        counter.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn heartbeat(&self) {
        self.last_heartbeat_unix
            .store(Utc::now().timestamp(), Ordering::Relaxed);
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
        };
        counter.load(Ordering::Relaxed)
    }

    /// Returns the last completed loop heartbeat as a Unix timestamp.
    #[must_use]
    pub fn last_heartbeat_unix(&self) -> i64 {
        self.last_heartbeat_unix.load(Ordering::Relaxed)
    }
}
