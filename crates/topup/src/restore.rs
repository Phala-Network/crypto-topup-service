//! Post-restore database validation.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::{Value, json};
use sqlx::{PgPool, Row as _};
use topup_adapters::settlement::http::{SettlementAnswer, SettlementApi};
use uuid::Uuid;

use crate::db::{self, MIGRATOR};

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
    pub post_restore_reconciliation: ReconciliationReport,
}

/// Product reconciliation completed before the restored service resumes.
#[derive(Debug, Serialize)]
pub struct ReconciliationReport {
    /// Indicates that every non-terminal settlement was queried successfully.
    pub status: &'static str,
    /// Deposits for which §13 requires a product GET before resume.
    pub deposits_at_or_beyond_cleared: i64,
    /// Non-terminal settlement records queried by idempotency key.
    pub non_terminal_settlements: i64,
    /// Product answers that made settlements terminally accepted.
    pub accepted: i64,
    /// Product answers that made settlements terminally rejected.
    pub rejected: i64,
    /// Product answers that remain in processing.
    pub processing: i64,
    /// Keys not yet known by the product and safe for the normal GET-first retry path.
    pub not_found: i64,
}

/// Creates a product settlement API for one configured endpoint.
pub type SettlementApiFactory<'a> =
    dyn Fn(&str) -> Result<Arc<dyn SettlementApi>, String> + Send + Sync + 'a;

/// Validates migration state, WAL application, heartbeat freshness, table counts, and the
/// post-restore reconciliation scope. The service must remain stopped while this runs.
pub async fn check(
    pool: &PgPool,
    client_factory: &SettlementApiFactory<'_>,
) -> Result<RestoreReport, String> {
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

    let post_restore_reconciliation = reconcile_settlements(pool, client_factory).await?;

    Ok(RestoreReport {
        status: "ok",
        latest_migration,
        in_recovery,
        last_replay_lsn,
        latest_applied_lsn,
        heartbeat_age_seconds,
        rpo_seconds,
        row_counts,
        post_restore_reconciliation,
    })
}

#[derive(Debug)]
struct ReconciliationCandidate {
    deposit_id: Uuid,
    key: String,
    payload: Value,
    settlement_url: String,
}

async fn reconcile_settlements(
    pool: &PgPool,
    client_factory: &SettlementApiFactory<'_>,
) -> Result<ReconciliationReport, String> {
    let deposits_at_or_beyond_cleared: i64 = sqlx::query_scalar(
        "SELECT count(*)::bigint FROM deposits WHERE state IN ('cleared', 'credited', 'swept')",
    )
    .fetch_one(pool)
    .await
    .map_err(|_| "failed to count deposits requiring restore reconciliation".to_owned())?;
    let rows = sqlx::query(
        "SELECT s.deposit_id, s.key, s.payload, p.settlement_url \
         FROM settlements s \
         JOIN products p ON p.id = s.product_id \
         WHERE s.status NOT IN ('accepted', 'rejected') \
         ORDER BY s.deposit_id",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| "failed to load settlements requiring restore reconciliation".to_owned())?;
    let non_terminal_settlements = i64::try_from(rows.len())
        .map_err(|_| "restore settlement reconciliation count overflowed".to_owned())?;
    let candidates = rows
        .into_iter()
        .map(|row| {
            Ok(ReconciliationCandidate {
                deposit_id: row
                    .try_get("deposit_id")
                    .map_err(|_| "restore settlement deposit identifier is invalid".to_owned())?,
                key: row
                    .try_get("key")
                    .map_err(|_| "restore settlement key is invalid".to_owned())?,
                payload: row
                    .try_get("payload")
                    .map_err(|_| "restore settlement payload is invalid".to_owned())?,
                settlement_url: row
                    .try_get("settlement_url")
                    .map_err(|_| "restore settlement product endpoint is invalid".to_owned())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut clients = BTreeMap::<String, Arc<dyn SettlementApi>>::new();
    let mut report = ReconciliationReport {
        status: "complete",
        deposits_at_or_beyond_cleared,
        non_terminal_settlements,
        accepted: 0,
        rejected: 0,
        processing: 0,
        not_found: 0,
    };
    for candidate in candidates {
        let client = match clients.get(&candidate.settlement_url) {
            Some(client) => Arc::clone(client),
            None => {
                let client = client_factory(&candidate.settlement_url)?;
                clients.insert(candidate.settlement_url.clone(), Arc::clone(&client));
                client
            }
        };
        let answer = client.get_by_key(&candidate.key).await.map_err(|_| {
            format!(
                "product lookup failed during restore reconciliation for deposit {}",
                candidate.deposit_id
            )
        })?;
        match answer {
            Some(SettlementAnswer::Accepted {
                destination_tx_id,
                payload,
            }) => {
                require_original_payload(&candidate, &payload)?;
                let receipt = json!({
                    "status": "accepted",
                    "destination_tx_id": &destination_tx_id,
                    "payload": payload,
                });
                db::mark_accepted(pool, candidate.deposit_id, &destination_tx_id, &receipt)
                    .await
                    .map_err(|_| "failed to persist accepted restore reconciliation".to_owned())?;
                report.accepted += 1;
            }
            Some(SettlementAnswer::Rejected { reason, payload }) => {
                require_original_payload(&candidate, &payload)?;
                let receipt = json!({
                    "status": "rejected",
                    "reason": reason,
                    "payload": payload,
                });
                db::mark_rejected(pool, candidate.deposit_id, &receipt)
                    .await
                    .map_err(|_| "failed to persist rejected restore reconciliation".to_owned())?;
                report.rejected += 1;
            }
            Some(SettlementAnswer::Processing { payload }) => {
                require_original_payload(&candidate, &payload)?;
                let receipt = json!({"status": "processing", "payload": payload});
                db::mark_sent_with_receipt(pool, candidate.deposit_id, &receipt)
                    .await
                    .map_err(|_| {
                        "failed to persist processing restore reconciliation".to_owned()
                    })?;
                report.processing += 1;
            }
            None => report.not_found += 1,
            Some(
                SettlementAnswer::Conflict409
                | SettlementAnswer::PayloadMismatch422
                | SettlementAnswer::Unknown { .. },
            ) => {
                return Err(format!(
                    "unexpected product answer during restore reconciliation for deposit {}",
                    candidate.deposit_id
                ));
            }
        }
    }
    Ok(report)
}

fn require_original_payload(
    candidate: &ReconciliationCandidate,
    product_payload: &Value,
) -> Result<(), String> {
    if product_payload == &candidate.payload {
        Ok(())
    } else {
        Err(format!(
            "product payload mismatch during restore reconciliation for deposit {}",
            candidate.deposit_id
        ))
    }
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
