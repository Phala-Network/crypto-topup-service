//! Post-restore database validation.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgPool, Row as _};

use crate::db::MIGRATOR;

/// Version of the newest migration embedded in this binary.
pub const LATEST_MIGRATION_VERSION: i64 = 20_260_922_000_011;

/// Successful restore validation report.
#[derive(Debug, Serialize)]
pub struct RestoreReport {
    /// Overall result.
    pub status: &'static str,
    /// Newest successful SQLx migration found in the restored database.
    pub latest_migration: i64,
    /// Whether PostgreSQL still reports archive recovery mode.
    pub in_recovery: bool,
    /// Most recent replayed WAL location, when PostgreSQL exposes one.
    pub last_replay_lsn: Option<String>,
    /// Latest replayed or current WAL location.
    pub latest_applied_lsn: String,
    /// Age of the newest restored heartbeat.
    pub heartbeat_age_seconds: i64,
    /// RPO recorded with the newest heartbeat.
    pub rpo_seconds: i32,
    /// Exact counts for durable service tables.
    pub row_counts: BTreeMap<&'static str, i64>,
    /// Restore-specific product reconciliation status.
    pub post_restore_reconciliation: ReconciliationHook,
}

/// Pending restore reconciliation until C6/C8 provide the product GET implementation.
#[derive(Debug, Serialize)]
pub struct ReconciliationHook {
    /// Indicates that this branch does not yet contain the reconciler.
    pub status: &'static str,
    /// Deposits for which §13 requires a product GET before resume.
    pub deposits_at_or_beyond_cleared: i64,
    /// Non-terminal settlement records that would be queried by idempotency key.
    pub non_terminal_settlements: i64,
    /// Operator-facing description of the deferred action.
    pub action: &'static str,
}

/// Validates migration state, WAL application, heartbeat freshness, table counts, and the
/// post-restore reconciliation scope.
pub async fn check(pool: &PgPool) -> Result<RestoreReport, String> {
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
    let latest_migration = expected
        .iter()
        .last()
        .map(|migration| migration.version)
        .ok_or_else(|| "binary contains no database migrations".to_owned())?;
    if latest_migration != LATEST_MIGRATION_VERSION {
        return Err("binary latest migration constant is stale".to_owned());
    }

    let wal = sqlx::query(
        "SELECT pg_is_in_recovery() AS in_recovery, \
         pg_last_wal_replay_lsn()::text AS last_replay_lsn, \
         COALESCE(pg_last_wal_replay_lsn(), pg_current_wal_lsn())::text AS latest_applied_lsn",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "failed to read WAL recovery state".to_owned())?;
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

    let heartbeat = sqlx::query(
        "SELECT recorded_at, rpo_seconds FROM heartbeat ORDER BY recorded_at DESC, id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| "failed to read restore heartbeat".to_owned())?
    .ok_or_else(|| "restore heartbeat table is empty".to_owned())?;
    let recorded_at: DateTime<Utc> = heartbeat
        .try_get("recorded_at")
        .map_err(|_| "restore heartbeat timestamp is invalid".to_owned())?;
    let rpo_seconds: i32 = heartbeat
        .try_get("rpo_seconds")
        .map_err(|_| "restore heartbeat RPO is invalid".to_owned())?;
    let heartbeat_age_seconds = Utc::now().signed_duration_since(recorded_at).num_seconds();
    if heartbeat_age_seconds < 0 {
        return Err("newest restore heartbeat is in the future".to_owned());
    }
    if heartbeat_age_seconds > i64::from(rpo_seconds) {
        return Err(format!(
            "restore RPO exceeded: heartbeat age {heartbeat_age_seconds}s > recorded {rpo_seconds}s"
        ));
    }

    let row_counts = row_counts(pool).await?;
    let deposits = *row_counts
        .get("deposits")
        .ok_or_else(|| "deposit row count is missing".to_owned())?;
    let settlements = *row_counts
        .get("settlements")
        .ok_or_else(|| "settlement row count is missing".to_owned())?;
    if settlements > deposits {
        return Err("row-count sanity failed: settlements exceed deposits".to_owned());
    }

    let reconciliation = sqlx::query(
        "SELECT \
         count(*) FILTER (WHERE state IN ('cleared', 'credited', 'swept'))::bigint AS deposits, \
         (SELECT count(*)::bigint FROM settlements WHERE status NOT IN ('accepted', 'rejected')) AS settlements \
         FROM deposits",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "failed to determine post-restore reconciliation scope".to_owned())?;

    Ok(RestoreReport {
        status: "ok",
        latest_migration,
        in_recovery,
        last_replay_lsn,
        latest_applied_lsn,
        heartbeat_age_seconds,
        rpo_seconds,
        row_counts,
        post_restore_reconciliation: ReconciliationHook {
            status: "hook_pending_c6_c8",
            deposits_at_or_beyond_cleared: reconciliation
                .try_get("deposits")
                .map_err(|_| "restore deposit reconciliation count is invalid".to_owned())?,
            non_terminal_settlements: reconciliation
                .try_get("settlements")
                .map_err(|_| "restore settlement reconciliation count is invalid".to_owned())?,
            action: "before resume, GET every listed product idempotency key; product answer wins",
        },
    })
}

async fn row_counts(pool: &PgPool) -> Result<BTreeMap<&'static str, i64>, String> {
    const TABLES: [&str; 14] = [
        "products",
        "accounts",
        "addresses",
        "rate_locks",
        "cursors",
        "deposits",
        "transitions",
        "settlements",
        "flushes",
        "flushed",
        "refunds",
        "outbox",
        "audit",
        "heartbeat",
    ];
    let row = sqlx::query(
        "SELECT \
         (SELECT count(*) FROM products)::bigint AS products, \
         (SELECT count(*) FROM accounts)::bigint AS accounts, \
         (SELECT count(*) FROM addresses)::bigint AS addresses, \
         (SELECT count(*) FROM rate_locks)::bigint AS rate_locks, \
         (SELECT count(*) FROM cursors)::bigint AS cursors, \
         (SELECT count(*) FROM deposits)::bigint AS deposits, \
         (SELECT count(*) FROM transitions)::bigint AS transitions, \
         (SELECT count(*) FROM settlements)::bigint AS settlements, \
         (SELECT count(*) FROM flushes)::bigint AS flushes, \
         (SELECT count(*) FROM flushed)::bigint AS flushed, \
         (SELECT count(*) FROM refunds)::bigint AS refunds, \
         (SELECT count(*) FROM outbox)::bigint AS outbox, \
         (SELECT count(*) FROM audit)::bigint AS audit, \
         (SELECT count(*) FROM heartbeat)::bigint AS heartbeat",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "failed to read durable table counts".to_owned())?;

    let mut counts = BTreeMap::new();
    for table in TABLES {
        let count = row
            .try_get(table)
            .map_err(|_| format!("row count for {table} is invalid"))?;
        counts.insert(table, count);
    }
    Ok(counts)
}
