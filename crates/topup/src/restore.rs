//! Post-restore database validation.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Row as _};
use topup_adapters::settlement::http::{SettlementAnswer, SettlementApi};
use topup_core::deposit::{DepositState, RejectReason, StepOutcome, next};
use uuid::Uuid;

use crate::db::{self, ApplyTransitionResult, MIGRATOR, SettlementIntent, TransitionUpdate};
use crate::pump::StepResult;
use crate::steps::settle::SettleStep;

/// Version of the newest migration embedded in this binary.
pub const LATEST_MIGRATION_VERSION: i64 = 20_260_922_000_011;
const HEARTBEAT_SAMPLING_SECONDS: i32 = 60;

/// Source-side failure point recorded outside the PostgreSQL volume being restored.
#[derive(Clone, Debug)]
pub struct RestoreExpectations {
    /// Last heartbeat known committed on the source immediately before destruction.
    pub expected_heartbeat_at: DateTime<Utc>,
    /// Source WAL insert location recorded at the same point.
    pub expected_lsn: String,
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
    /// Externally recorded source WAL location.
    pub expected_lsn: String,
    /// WAL bytes between the external source point and the restored replay point.
    pub wal_bytes_behind: i64,
    /// Externally recorded last committed source heartbeat.
    pub expected_heartbeat_at: DateTime<Utc>,
    /// Newest heartbeat present after restore.
    pub restored_heartbeat_at: DateTime<Utc>,
    /// Data loss measured between source and restored heartbeat samples.
    pub measured_rpo_seconds: i64,
    /// Maximum accepted loss between committed heartbeat samples.
    pub allowed_rpo_seconds: i32,
    /// RPO target recorded by the heartbeat schema before sampling tolerance.
    pub recorded_rpo_seconds: i32,
    /// Heartbeat sampling interval used when interpreting RPO.
    pub heartbeat_sampling_seconds: i32,
    /// Exact counts for durable service tables.
    pub row_counts: BTreeMap<&'static str, i64>,
    /// Restore-specific product reconciliation status.
    pub post_restore_reconciliation: ReconciliationReport,
}

/// Product reconciliation completed before the restored service resumes.
#[derive(Debug, Serialize)]
pub struct ReconciliationReport {
    /// `complete` only when every required product GET was adopted.
    pub status: &'static str,
    /// Deposits for which architecture section 13 requires a product GET before resume.
    pub deposits_at_or_beyond_cleared: i64,
    /// Product GET requests attempted by idempotency key.
    pub settlements_queried: i64,
    /// Product answers adopted as accepted.
    pub accepted: i64,
    /// Product answers adopted as rejected.
    pub rejected: i64,
    /// Deposits still processing and therefore unsafe to resume.
    pub processing: i64,
    /// Product keys not found and therefore unsafe to resume.
    pub not_found: i64,
    /// Per-deposit failures that make this report incomplete.
    pub failures: Vec<String>,
}

/// Creates a product settlement API for one configured endpoint.
pub type SettlementApiFactory<'a> =
    dyn Fn(&str) -> Result<Arc<dyn SettlementApi>, String> + Send + Sync + 'a;

/// Validates migration state, WAL application, externally anchored RPO, table counts, and product
/// reconciliation. The service must remain stopped while this runs.
pub async fn check(
    pool: &PgPool,
    expectations: &RestoreExpectations,
    client_factory: &SettlementApiFactory<'_>,
) -> Result<RestoreReport, String> {
    let latest_migration = check_migrations(pool).await?;

    let wal = sqlx::query(
        "SELECT pg_is_in_recovery() AS in_recovery, \
         pg_last_wal_replay_lsn()::text AS last_replay_lsn, \
         COALESCE(pg_last_wal_replay_lsn(), pg_current_wal_lsn())::text AS latest_applied_lsn, \
         $1::pg_lsn::text AS expected_lsn, \
         GREATEST(pg_wal_lsn_diff($1::pg_lsn, \
             COALESCE(pg_last_wal_replay_lsn(), pg_current_wal_lsn())), 0)::bigint \
             AS wal_bytes_behind",
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
    let expected_lsn: String = wal
        .try_get("expected_lsn")
        .map_err(|_| "expected WAL LSN is invalid".to_owned())?;
    let wal_bytes_behind: i64 = wal
        .try_get("wal_bytes_behind")
        .map_err(|_| "WAL distance is invalid".to_owned())?;

    let heartbeat = sqlx::query(
        "SELECT recorded_at, rpo_seconds FROM heartbeat ORDER BY recorded_at DESC, id DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await
    .map_err(|_| "failed to read restore heartbeat".to_owned())?
    .ok_or_else(|| "restore heartbeat table is empty".to_owned())?;
    let restored_heartbeat_at: DateTime<Utc> = heartbeat
        .try_get("recorded_at")
        .map_err(|_| "restore heartbeat timestamp is invalid".to_owned())?;
    let recorded_rpo_seconds: i32 = heartbeat
        .try_get("rpo_seconds")
        .map_err(|_| "restore heartbeat RPO is invalid".to_owned())?;
    let allowed_rpo_seconds = recorded_rpo_seconds
        .checked_add(HEARTBEAT_SAMPLING_SECONDS)
        .ok_or_else(|| "restore heartbeat RPO tolerance overflowed".to_owned())?;
    let measured_rpo_seconds = expectations
        .expected_heartbeat_at
        .signed_duration_since(restored_heartbeat_at)
        .num_seconds()
        .max(0);

    let post_restore_reconciliation = reconcile_settlements(pool, client_factory).await?;
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

    let mut failures = Vec::new();
    if measured_rpo_seconds > i64::from(allowed_rpo_seconds) {
        failures.push(format!(
            "restore RPO exceeded: measured {measured_rpo_seconds}s > allowed {allowed_rpo_seconds}s"
        ));
    }
    failures.extend(post_restore_reconciliation.failures.iter().cloned());
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
        expected_heartbeat_at: expectations.expected_heartbeat_at,
        restored_heartbeat_at,
        measured_rpo_seconds,
        allowed_rpo_seconds,
        recorded_rpo_seconds,
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
    let latest = expected
        .last()
        .map(|migration| migration.version)
        .ok_or_else(|| "binary contains no database migrations".to_owned())?;
    if latest != LATEST_MIGRATION_VERSION {
        return Err("binary latest migration constant is stale".to_owned());
    }
    Ok(latest)
}

#[derive(Debug)]
struct ReconciliationCandidate {
    deposit_id: Uuid,
    product_id: Uuid,
    account_external_id: String,
    settlement_url: String,
    settlement_key: Option<String>,
    settlement_product_id: Option<Uuid>,
}

async fn reconcile_settlements(
    pool: &PgPool,
    client_factory: &SettlementApiFactory<'_>,
) -> Result<ReconciliationReport, String> {
    let rows = sqlx::query(
        "SELECT d.id AS deposit_id, p.id AS product_id, a.external_id, p.settlement_url, \
         s.key AS settlement_key, s.product_id AS settlement_product_id \
         FROM deposits d \
         JOIN accounts a ON a.id = d.account_id \
         JOIN products p ON p.id = a.product_id \
         LEFT JOIN settlements s ON s.deposit_id = d.id \
         WHERE d.state IN ('cleared', 'credited', 'swept') \
            OR (d.state = 'rejected' AND d.reason = 'product_refused') \
         ORDER BY d.id",
    )
    .fetch_all(pool)
    .await
    .map_err(|_| "failed to load deposits requiring restore reconciliation".to_owned())?;
    let deposits_at_or_beyond_cleared = i64::try_from(rows.len())
        .map_err(|_| "restore settlement reconciliation count overflowed".to_owned())?;
    let candidates = rows
        .into_iter()
        .map(|row| {
            Ok(ReconciliationCandidate {
                deposit_id: row
                    .try_get("deposit_id")
                    .map_err(|_| "restore settlement deposit identifier is invalid".to_owned())?,
                product_id: row
                    .try_get("product_id")
                    .map_err(|_| "restore settlement product identifier is invalid".to_owned())?,
                account_external_id: row
                    .try_get("external_id")
                    .map_err(|_| "restore settlement account identifier is invalid".to_owned())?,
                settlement_url: row
                    .try_get("settlement_url")
                    .map_err(|_| "restore settlement product endpoint is invalid".to_owned())?,
                settlement_key: row
                    .try_get("settlement_key")
                    .map_err(|_| "restore settlement key is invalid".to_owned())?,
                settlement_product_id: row
                    .try_get("settlement_product_id")
                    .map_err(|_| "restore settlement product link is invalid".to_owned())?,
            })
        })
        .collect::<Result<Vec<_>, String>>()?;

    let mut clients = BTreeMap::<String, Arc<dyn SettlementApi>>::new();
    let mut report = ReconciliationReport {
        status: "complete",
        deposits_at_or_beyond_cleared,
        settlements_queried: 0,
        accepted: 0,
        rejected: 0,
        processing: 0,
        not_found: 0,
        failures: Vec::new(),
    };
    for candidate in candidates {
        reconcile_candidate(pool, client_factory, &mut clients, &candidate, &mut report).await;
    }
    if !report.failures.is_empty() {
        report.status = "incomplete";
    }
    Ok(report)
}

async fn reconcile_candidate(
    pool: &PgPool,
    client_factory: &SettlementApiFactory<'_>,
    clients: &mut BTreeMap<String, Arc<dyn SettlementApi>>,
    candidate: &ReconciliationCandidate,
    report: &mut ReconciliationReport,
) {
    let key = format!("deposit:{}", candidate.deposit_id);
    if candidate
        .settlement_key
        .as_deref()
        .is_some_and(|stored| stored != key)
    {
        report.failures.push(format!(
            "deposit {} has a non-canonical settlement key",
            candidate.deposit_id
        ));
        return;
    }
    if candidate
        .settlement_product_id
        .is_some_and(|stored| stored != candidate.product_id)
    {
        report.failures.push(format!(
            "deposit {} has a settlement linked to the wrong product",
            candidate.deposit_id
        ));
        return;
    }
    let client = match clients.get(&candidate.settlement_url) {
        Some(client) => Arc::clone(client),
        None => match client_factory(&candidate.settlement_url) {
            Ok(client) => {
                clients.insert(candidate.settlement_url.clone(), Arc::clone(&client));
                client
            }
            Err(_) => {
                report.failures.push(format!(
                    "product client creation failed for deposit {}",
                    candidate.deposit_id
                ));
                return;
            }
        },
    };
    report.settlements_queried += 1;
    let answer = match client.get_by_key(&key).await {
        Ok(Some(answer)) => answer,
        Ok(None) => {
            report.not_found += 1;
            report.failures.push(format!(
                "product has no settlement for deposit {}",
                candidate.deposit_id
            ));
            return;
        }
        Err(_) => {
            report.failures.push(format!(
                "product lookup failed for deposit {}",
                candidate.deposit_id
            ));
            return;
        }
    };
    let deposit = match db::get_deposit(pool, candidate.deposit_id).await {
        Ok(Some(deposit)) => deposit,
        Ok(None) | Err(_) => {
            report.failures.push(format!(
                "deposit {} disappeared during reconciliation",
                candidate.deposit_id
            ));
            return;
        }
    };
    if candidate.settlement_key.is_none() {
        let Some(payload) = answer_payload(&answer) else {
            report.failures.push(format!(
                "deposit {} has no settlement row and product answer has no recoverable payload",
                candidate.deposit_id
            ));
            return;
        };
        if db::upsert_intent(
            pool,
            &SettlementIntent {
                deposit_id: deposit.id,
                product_id: candidate.product_id,
                key: key.clone(),
                payload,
            },
        )
        .await
        .is_err()
        {
            report.failures.push(format!(
                "failed to recreate settlement intent for deposit {}",
                candidate.deposit_id
            ));
            return;
        }
    }

    let result = match SettleStep::adopt_product_answer(
        pool,
        &deposit,
        candidate.product_id,
        &candidate.account_external_id,
        answer,
    )
    .await
    {
        Ok(result) => result,
        Err(_) => {
            report.failures.push(format!(
                "product answer failed identity verification or persistence for deposit {}",
                candidate.deposit_id
            ));
            return;
        }
    };
    match result.outcome {
        StepOutcome::Advance => {
            report.accepted += 1;
            match deposit.state {
                DepositState::Cleared => {
                    if apply_restore_transition(pool, &deposit, &result)
                        .await
                        .is_err()
                    {
                        report.failures.push(format!(
                            "failed to advance accepted deposit {}",
                            candidate.deposit_id
                        ));
                    }
                }
                DepositState::Credited | DepositState::Swept => {}
                DepositState::Rejected => report.failures.push(format!(
                    "product accepted locally rejected deposit {}; manual repair is required",
                    candidate.deposit_id
                )),
                DepositState::Detected | DepositState::Confirmed => report.failures.push(format!(
                    "deposit {} was selected below the cleared state",
                    candidate.deposit_id
                )),
            }
        }
        StepOutcome::Reject(RejectReason::ProductRefused) => {
            report.rejected += 1;
            match deposit.state {
                DepositState::Cleared => {
                    if apply_restore_transition(pool, &deposit, &result)
                        .await
                        .is_err()
                    {
                        report
                            .failures
                            .push(format!("failed to reject deposit {}", candidate.deposit_id));
                    }
                }
                DepositState::Rejected if deposit.reason == Some(RejectReason::ProductRefused) => {}
                DepositState::Credited | DepositState::Swept => report.failures.push(format!(
                    "product rejected deposit {} after local credit; manual repair is required",
                    candidate.deposit_id
                )),
                DepositState::Rejected => report.failures.push(format!(
                    "product answer conflicts with the local rejection reason for deposit {}",
                    candidate.deposit_id
                )),
                DepositState::Detected | DepositState::Confirmed => report.failures.push(format!(
                    "deposit {} was selected below the cleared state",
                    candidate.deposit_id
                )),
            }
        }
        StepOutcome::Wait { .. } => {
            report.processing += 1;
            report.failures.push(format!(
                "product settlement is still processing for deposit {}",
                candidate.deposit_id
            ));
        }
        StepOutcome::Retry { .. }
        | StepOutcome::AdoptProductAnswer { .. }
        | StepOutcome::Reject(_) => {
            report.failures.push(format!(
                "product returned a non-adoptable answer for deposit {}",
                candidate.deposit_id
            ));
        }
    }
}

fn answer_payload(answer: &SettlementAnswer) -> Option<Value> {
    match answer {
        SettlementAnswer::Accepted { payload, .. }
        | SettlementAnswer::Processing { payload }
        | SettlementAnswer::Rejected { payload, .. } => Some(payload.clone()),
        SettlementAnswer::Conflict409
        | SettlementAnswer::PayloadMismatch422
        | SettlementAnswer::Unknown { .. } => None,
    }
}

async fn apply_restore_transition(
    pool: &PgPool,
    deposit: &db::Deposit,
    result: &StepResult,
) -> Result<(), String> {
    let transition = next(deposit.state, &result.outcome)
        .map_err(|_| "product answer cannot be applied to restored deposit state".to_owned())?;
    let rejection_reason = match result.outcome {
        StepOutcome::Reject(reason) => Some(reason),
        _ => None,
    };
    let lease_token = Uuid::new_v4();
    let mut transaction = pool
        .begin()
        .await
        .map_err(|_| "failed to start restore transition".to_owned())?;
    let claimed = sqlx::query_scalar::<_, Uuid>(
        "UPDATE deposits SET lease_token = $2, lease_until = now() + interval '5 minutes' \
         WHERE id = $1 AND state = $3 RETURNING id",
    )
    .bind(deposit.id)
    .bind(lease_token)
    .bind(db::state_code(deposit.state))
    .fetch_optional(&mut *transaction)
    .await
    .map_err(|_| "failed to lock restored deposit".to_owned())?;
    if claimed.is_none() {
        return Err("restored deposit state changed during reconciliation".to_owned());
    }
    let applied = db::apply_transition(
        &mut transaction,
        deposit.id,
        deposit.state,
        lease_token,
        TransitionUpdate {
            transition,
            rejection_reason,
            attempt: 0,
            next_attempt_at: Utc::now(),
        },
        &result.evidence,
        &result.events,
    )
    .await
    .map_err(|_| "failed to persist restored deposit transition".to_owned())?;
    if applied != ApplyTransitionResult::Applied {
        return Err("restored deposit transition became stale".to_owned());
    }
    transaction
        .commit()
        .await
        .map_err(|_| "failed to commit restored deposit transition".to_owned())
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
