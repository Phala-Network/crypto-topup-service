//! Post-restore database validation.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{AssertSqlSafe, PgPool, Row as _};

use crate::db::MIGRATOR;
use crate::heartbeat::RPO_SECONDS;
use crate::reconciler::{CheckName, Finding, Reconciler};

const HEARTBEAT_SAMPLING_SECONDS: i32 = 60;
/// A committed heartbeat can be up to one sampling interval older than the failure point.
const ALLOWED_RPO_SECONDS: i32 = RPO_SECONDS + HEARTBEAT_SAMPLING_SECONDS;

/// Source-side failure point recorded outside the PostgreSQL volume being restored.
#[derive(Clone, Debug)]
pub struct RestoreExpectations {
    /// Last heartbeat known committed on the source immediately before destruction. `None` when
    /// the check runs at boot without one: the report then leaves the RPO comparison against the
    /// operator's external anchor to the operator (`rpo_basis` `unanchored`).
    pub expected_heartbeat_at: Option<DateTime<Utc>>,
    /// Source WAL location logged with that heartbeat; `None` is a declared incident exception.
    pub expected_lsn: Option<String>,
}

/// Restore validation report. `incomplete` is a hard pre-resume failure.
#[derive(Debug, Serialize)]
pub struct RestoreReport {
    /// Overall result: `ok` or `incomplete`.
    pub status: &'static str,
    /// Reasons that prevent resuming service traffic.
    pub failures: Vec<String>,
    /// Newest successful SQLx migration found in the restored database.
    pub latest_migration: i64,
    /// Whether PostgreSQL still reports archive recovery mode.
    pub in_recovery: bool,
    /// Most recent replayed WAL location, when PostgreSQL exposes one.
    pub last_replay_lsn: Option<String>,
    /// Latest replayed or current WAL location.
    pub latest_applied_lsn: String,
    /// Externally recorded source WAL location, when one was available.
    pub expected_lsn: Option<String>,
    /// WAL bytes between the external source point and the restored replay point.
    pub wal_bytes_behind: Option<i64>,
    /// `heartbeat_and_lsn`, `heartbeat_only` when no source LSN was supplied and the RPO rests
    /// on the heartbeat timestamp alone, or `unanchored` when no source heartbeat was supplied and
    /// the operator compares `restored_heartbeat_at` with their own external anchor.
    pub rpo_basis: &'static str,
    /// Externally recorded last committed source heartbeat, when one was supplied.
    pub expected_heartbeat_at: Option<DateTime<Utc>>,
    /// Newest heartbeat present after restore.
    pub restored_heartbeat_at: DateTime<Utc>,
    /// Data loss measured between source and restored heartbeat samples, when anchored.
    pub measured_rpo_seconds: Option<i64>,
    /// Maximum accepted loss between committed heartbeat samples.
    pub allowed_rpo_seconds: i32,
    /// RPO target before sampling tolerance.
    pub rpo_seconds: i32,
    /// Heartbeat sampling interval used when interpreting RPO.
    pub heartbeat_sampling_seconds: i32,
    /// Exact counts for durable service tables.
    pub row_counts: BTreeMap<&'static str, i64>,
    /// Result of the library post-restore reconciliation round.
    pub post_restore_reconciliation: PostRestoreReconciliation,
}

/// Outcome of the §13 post-restore reconciliation round.
#[derive(Debug, Serialize)]
pub struct PostRestoreReconciliation {
    /// `complete` unless a finding left a subject unverified.
    pub status: &'static str,
    /// Checks that could not finish; they alert like any round and do not gate resume.
    pub failed_checks: Vec<CheckName>,
    /// Every finding of the round, including alert-only findings from the regular checks.
    pub findings: Vec<Finding>,
}

/// Validates migration state, WAL application, externally anchored RPO, and table counts, then runs
/// the §13 post-restore reconciliation through [`Reconciler::post_restore_once`].
///
/// The service, heartbeat, and backup processes must remain stopped while this runs: the
/// post-restore round holds the lease-owner lock and may repair the restored ledger. It asks the
/// product nothing: the service's own record is authoritative for its credits.
pub async fn check(
    pool: &PgPool,
    expectations: &RestoreExpectations,
    reconciler: &Reconciler,
) -> Result<RestoreReport, String> {
    let latest_migration = check_migrations(pool).await?;

    let wal = sqlx::query(
        "SELECT pg_is_in_recovery() AS in_recovery, \
         pg_last_wal_replay_lsn()::text AS last_replay_lsn, \
         COALESCE(pg_last_wal_replay_lsn(), pg_current_wal_lsn())::text AS latest_applied_lsn, \
         $1::text::pg_lsn::text AS expected_lsn, \
         CASE WHEN $1::text IS NULL THEN NULL \
             ELSE GREATEST(pg_wal_lsn_diff($1::text::pg_lsn, \
                 COALESCE(pg_last_wal_replay_lsn(), pg_current_wal_lsn())), 0)::bigint \
         END AS wal_bytes_behind",
    )
    .bind(&expectations.expected_lsn)
    .fetch_one(pool)
    .await
    .map_err(|_| "failed to read or compare WAL recovery state".to_owned())?;
    let in_recovery: bool = wal
        .try_get("in_recovery")
        .map_err(|_| "WAL recovery state is invalid".to_owned())?;
    if in_recovery {
        return Err("PostgreSQL is still in archive recovery".to_owned());
    }
    let last_replay_lsn: Option<String> = wal
        .try_get("last_replay_lsn")
        .map_err(|_| "last replay LSN is invalid".to_owned())?;
    let latest_applied_lsn: String = wal
        .try_get("latest_applied_lsn")
        .map_err(|_| "latest applied WAL LSN is unavailable".to_owned())?;
    let expected_lsn: Option<String> = wal
        .try_get("expected_lsn")
        .map_err(|_| "expected WAL LSN is invalid".to_owned())?;
    let wal_bytes_behind: Option<i64> = wal
        .try_get("wal_bytes_behind")
        .map_err(|_| "WAL distance is invalid".to_owned())?;

    let heartbeat =
        sqlx::query("SELECT recorded_at FROM heartbeat ORDER BY recorded_at DESC, id DESC LIMIT 1")
            .fetch_optional(pool)
            .await
            .map_err(|_| "failed to read restore heartbeat".to_owned())?
            .ok_or_else(|| "restore heartbeat table is empty".to_owned())?;
    let restored_heartbeat_at: DateTime<Utc> = heartbeat
        .try_get("recorded_at")
        .map_err(|_| "restore heartbeat timestamp is invalid".to_owned())?;
    let measured_rpo_seconds = expectations.expected_heartbeat_at.map(|expected| {
        expected
            .signed_duration_since(restored_heartbeat_at)
            .num_seconds()
            .max(0)
    });

    let round = reconciler
        .post_restore_once()
        .await
        .map_err(|error| format!("post-restore reconciliation failed: {}", error.code()))?;
    let post_restore_reconciliation = PostRestoreReconciliation {
        status: if round.incomplete {
            "incomplete"
        } else {
            "complete"
        },
        failed_checks: round.failed_checks,
        findings: round.findings,
    };
    let row_counts = row_counts(pool).await?;

    let mut failures = Vec::new();
    if let Some(measured) = measured_rpo_seconds
        && measured > i64::from(ALLOWED_RPO_SECONDS)
    {
        failures.push(format!(
            "restore RPO exceeded: measured {measured}s > allowed {ALLOWED_RPO_SECONDS}s"
        ));
    }
    failures.extend(
        post_restore_reconciliation
            .findings
            .iter()
            .filter(|finding| finding.incomplete)
            .map(|finding| {
                let subject = finding
                    .subjects
                    .get("deposit_id")
                    .map_or("unknown", String::as_str);
                format!("post-restore reconciliation is incomplete for deposit {subject}")
            }),
    );
    let status = if failures.is_empty() {
        "ok"
    } else {
        "incomplete"
    };

    Ok(RestoreReport {
        status,
        failures,
        latest_migration,
        in_recovery,
        last_replay_lsn,
        latest_applied_lsn,
        expected_lsn,
        wal_bytes_behind,
        rpo_basis: match (
            expectations.expected_heartbeat_at,
            &expectations.expected_lsn,
        ) {
            (None, _) => "unanchored",
            (Some(_), Some(_)) => "heartbeat_and_lsn",
            (Some(_), None) => "heartbeat_only",
        },
        expected_heartbeat_at: expectations.expected_heartbeat_at,
        restored_heartbeat_at,
        measured_rpo_seconds,
        allowed_rpo_seconds: ALLOWED_RPO_SECONDS,
        rpo_seconds: RPO_SECONDS,
        heartbeat_sampling_seconds: HEARTBEAT_SAMPLING_SECONDS,
        row_counts,
        post_restore_reconciliation,
    })
}

async fn check_migrations(pool: &PgPool) -> Result<i64, String> {
    let applied =
        sqlx::query("SELECT version, checksum, success FROM _sqlx_migrations ORDER BY version")
            .fetch_all(pool)
            .await
            .map_err(|_| "failed to read migration state".to_owned())?;
    let expected = MIGRATOR
        .iter()
        .filter(|migration| migration.migration_type.is_up_migration())
        .collect::<Vec<_>>();
    let expected_count = expected.len();
    if applied.len() != expected_count {
        return Err(format!(
            "schema migration count mismatch: applied={}, expected={expected_count}",
            applied.len()
        ));
    }
    for (row, expected) in applied.iter().zip(expected.iter()) {
        let version: i64 = row
            .try_get("version")
            .map_err(|_| "migration version is invalid".to_owned())?;
        let checksum: Vec<u8> = row
            .try_get("checksum")
            .map_err(|_| format!("migration {version} checksum is invalid"))?;
        let success: bool = row
            .try_get("success")
            .map_err(|_| format!("migration {version} success flag is invalid"))?;
        if version != expected.version
            || !success
            || checksum.as_slice() != expected.checksum.as_ref()
        {
            return Err(format!(
                "schema migration mismatch at version {version}; expected {}",
                expected.version
            ));
        }
    }
    expected
        .last()
        .map(|migration| migration.version)
        .ok_or_else(|| "binary contains no database migrations".to_owned())
}

/// Durable tables whose exact counts are reported; identifiers are constants, never input.
const COUNTED_TABLES: [&str; 17] = [
    "accounts",
    "customers",
    "quotes",
    "deposit_addresses",
    "addresses",
    "cursors",
    "deposits",
    "transitions",
    "flushed",
    "flush_failures",
    "refunds",
    "events",
    "webhook_deliveries",
    "audit",
    "reconciliation_findings",
    "reconciliation_blocks",
    "heartbeat",
];

async fn row_counts(pool: &PgPool) -> Result<BTreeMap<&'static str, i64>, String> {
    let columns = COUNTED_TABLES
        .iter()
        .map(|table| format!("(SELECT count(*) FROM {table})::bigint AS {table}"))
        .collect::<Vec<_>>()
        .join(", ");
    let row = sqlx::query(AssertSqlSafe(format!("SELECT {columns}")))
        .fetch_one(pool)
        .await
        .map_err(|_| "failed to read durable table counts".to_owned())?;

    let mut counts = BTreeMap::new();
    for table in COUNTED_TABLES {
        let count = row
            .try_get(table)
            .map_err(|_| format!("row count for {table} is invalid"))?;
        counts.insert(table, count);
    }
    Ok(counts)
}
