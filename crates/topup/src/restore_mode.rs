//! Restore mode (docs/architecture.md §14, docs/design/multi-tenant.md §13).
//!
//! A database restored from backup holds the backup's state, not the business's: within the RPO
//! window it can have lost deposit addresses given to customers, API key revocations (the keys
//! work again), treasury cancellations, webhook endpoint changes, and events the merchant already
//! received. So the service starts **frozen** after a restore: merchant writes answer
//! `503 service_restoring`, and nothing credits, settles, expires a quote, applies a treasury
//! change, verifies a refund, or delivers an event, while reads, health, the scanner (the rescan
//! from the restored cursor), and the reconciler run. The operator reconciles through
//! `/v1/admin/restore/…` and unfreezes (`deploy/RESTORE.md`).
//!
//! A restore is known two ways: `restore-check`, which runs only in the restore-check variant
//! after a restore on boot, records it ([`freeze_after_restore`]); and `topup run` finds a
//! PostgreSQL timeline newer than the one acknowledged ([`detect`]), since every promotion out of
//! archive recovery starts a new timeline and crash recovery does not. Either way the freeze is a
//! row of `restores`, so it survives the upgrade from the restore-check variant to the service.

use std::collections::BTreeMap;
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde_json::Value;
use sqlx::types::Json;
use sqlx::{FromRow, PgConnection, PgPool};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use crate::audit::{self, Actor};
use crate::routes::RouteSet;

/// The current WAL insert timeline, from the name of the current WAL file.
const CURRENT_TIMELINE: &str =
    "('x' || left(pg_walfile_name(pg_current_wal_insert_lsn()), 8))::bit(32)::bigint";

/// How often a held service task checks whether the freeze was lifted.
pub const UNFREEZE_POLL_INTERVAL: Duration = Duration::from_secs(5);

/// The deposit events a rescan re-derives, whose object is the deposit and whose id is
/// `event_id(type, deposit)`: the only events [`import_delivered_event`] accepts.
pub const REDERIVED_EVENT_TYPES: [&str; 3] =
    ["deposit.credited", "deposit.rejected", "deposit.reversed"];

/// How a restore was detected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Detection {
    /// `restore-check` ran after a restore on boot of the restore-check variant.
    RestoreCheck,
    /// `topup run` found a PostgreSQL timeline newer than the acknowledged one.
    Timeline,
}

impl Detection {
    /// The stored code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RestoreCheck => "restore_check",
            Self::Timeline => "timeline",
        }
    }
}

/// A detected restore; frozen until `unfrozen_at` is set.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Restore {
    /// Restore id.
    pub id: Uuid,
    /// When the service found the restore and froze.
    pub detected_at: DateTime<Utc>,
    /// `restore_check` or `timeline`.
    pub detected_by: String,
    /// The PostgreSQL timeline the restore promoted to.
    pub timeline_id: i64,
    /// Newest heartbeat in the restored database: changes after it may be lost.
    pub restore_point: Option<DateTime<Utc>>,
    /// Each chain's scanned block when the restore was detected, where the rescan starts.
    pub restored_cursors: BTreeMap<u64, u64>,
    /// When the operator unfroze the service.
    pub unfrozen_at: Option<DateTime<Utc>>,
    /// Who unfroze it.
    pub unfrozen_by: Option<String>,
    /// Why, with the operator's checklist.
    pub unfreeze_reason: Option<String>,
}

#[derive(FromRow)]
struct RestoreRow {
    id: Uuid,
    detected_at: DateTime<Utc>,
    detected_by: String,
    timeline_id: i64,
    restore_point: Option<DateTime<Utc>>,
    restored_cursors: Json<BTreeMap<String, i64>>,
    unfrozen_at: Option<DateTime<Utc>>,
    unfrozen_by: Option<String>,
    unfreeze_reason: Option<String>,
}

impl TryFrom<RestoreRow> for Restore {
    type Error = sqlx::Error;

    fn try_from(row: RestoreRow) -> Result<Self, Self::Error> {
        let invalid = || sqlx::Error::Decode("restores.restored_cursors is invalid".into());
        let restored_cursors = row
            .restored_cursors
            .0
            .into_iter()
            .map(|(chain_id, block)| {
                Ok((
                    chain_id.parse::<u64>().map_err(|_| invalid())?,
                    u64::try_from(block).map_err(|_| invalid())?,
                ))
            })
            .collect::<Result<_, sqlx::Error>>()?;
        Ok(Self {
            id: row.id,
            detected_at: row.detected_at,
            detected_by: row.detected_by,
            timeline_id: row.timeline_id,
            restore_point: row.restore_point,
            restored_cursors,
            unfrozen_at: row.unfrozen_at,
            unfrozen_by: row.unfrozen_by,
            unfreeze_reason: row.unfreeze_reason,
        })
    }
}

const RESTORE_COLUMNS: &str = "id, detected_at, detected_by, timeline_id, restore_point, \
     restored_cursors, unfrozen_at, unfrozen_by, unfreeze_reason";

/// Records the restore `restore-check` runs after, freezing the service unless a freeze is
/// already in place, and acknowledges the current timeline.
pub async fn freeze_after_restore(pool: &PgPool) -> Result<Restore, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let (_, current) = lock_timeline(&mut transaction).await?;
    let restore = freeze_in(&mut transaction, Detection::RestoreCheck, current).await?;
    acknowledge(&mut transaction, current).await?;
    transaction.commit().await?;
    Ok(restore)
}

/// Freezes the service when PostgreSQL runs on a timeline newer than the acknowledged one (a
/// restore that booted straight into the service), and returns the active freeze, if any.
pub async fn detect(pool: &PgPool) -> Result<Option<Restore>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let (acknowledged, current) = lock_timeline(&mut transaction).await?;
    if current > acknowledged {
        freeze_in(&mut transaction, Detection::Timeline, current).await?;
        acknowledge(&mut transaction, current).await?;
    }
    let active = active_in(&mut transaction).await?;
    transaction.commit().await?;
    Ok(active)
}

/// The acknowledged and the current timeline, with the acknowledged one locked.
async fn lock_timeline(connection: &mut PgConnection) -> Result<(i64, i64), sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT timeline_id, {CURRENT_TIMELINE} FROM restore_timeline FOR UPDATE"
    )))
    .fetch_one(connection)
    .await
}

async fn acknowledge(connection: &mut PgConnection, timeline: i64) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE restore_timeline SET timeline_id = GREATEST(timeline_id, $1)")
        .bind(timeline)
        .execute(connection)
        .await?;
    Ok(())
}

/// The active freeze, or a new one recording the restore point and each chain's cursor.
async fn freeze_in(
    connection: &mut PgConnection,
    detection: Detection,
    timeline: i64,
) -> Result<Restore, sqlx::Error> {
    if let Some(active) = active_in(connection).await? {
        return Ok(active);
    }
    let restore: Restore = sqlx::query_as::<_, RestoreRow>(sqlx::AssertSqlSafe(format!(
        r#"
        INSERT INTO restores (id, detected_by, timeline_id, restore_point, restored_cursors)
        SELECT $1, $2, $3,
               (SELECT max(recorded_at) FROM heartbeat),
               COALESCE((SELECT jsonb_object_agg(chain_id::text, scanned_block) FROM cursors),
                        '{{}}'::jsonb)
        RETURNING {RESTORE_COLUMNS}
        "#
    )))
    .bind(Uuid::new_v4())
    .bind(detection.code())
    .bind(timeline)
    .fetch_one(&mut *connection)
    .await?
    .try_into()?;
    tracing::error!(
        restore_id = %restore.id,
        detected_by = detection.code(),
        timeline,
        restore_point = ?restore.restore_point,
        "database restored from backup: the service is frozen until an operator reconciles and \
         unfreezes it (deploy/RESTORE.md)"
    );
    Ok(restore)
}

async fn active_in(connection: &mut PgConnection) -> Result<Option<Restore>, sqlx::Error> {
    sqlx::query_as::<_, RestoreRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {RESTORE_COLUMNS} FROM restores WHERE unfrozen_at IS NULL FOR UPDATE"
    )))
    .fetch_optional(connection)
    .await?
    .map(Restore::try_from)
    .transpose()
}

/// The active freeze, if any.
pub async fn active(pool: &PgPool) -> Result<Option<Restore>, sqlx::Error> {
    latest_matching(pool, "WHERE unfrozen_at IS NULL").await
}

/// The most recent restore, frozen or not.
pub async fn latest(pool: &PgPool) -> Result<Option<Restore>, sqlx::Error> {
    latest_matching(pool, "").await
}

async fn latest_matching(
    pool: &PgPool,
    filter: &'static str,
) -> Result<Option<Restore>, sqlx::Error> {
    sqlx::query_as::<_, RestoreRow>(sqlx::AssertSqlSafe(format!(
        "SELECT {RESTORE_COLUMNS} FROM restores {filter} ORDER BY detected_at DESC, id LIMIT 1"
    )))
    .fetch_optional(pool)
    .await?
    .map(Restore::try_from)
    .transpose()
}

/// Whether the service is frozen after a restore.
pub async fn is_frozen(pool: &PgPool) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM restores WHERE unfrozen_at IS NULL)")
        .fetch_one(pool)
        .await
}

/// Waits until the service is not frozen; `false` when `cancellation` fired first. A failed check
/// is logged and retried: a task held here starts only once a check shows no freeze.
pub async fn wait_until_unfrozen(
    pool: &PgPool,
    poll: Duration,
    cancellation: &CancellationToken,
) -> bool {
    let mut announced = false;
    loop {
        match is_frozen(pool).await {
            Ok(false) => return true,
            Ok(true) if !announced => {
                announced = true;
                tracing::info!("held while the service is frozen after a restore");
            }
            Ok(true) => {}
            Err(error) => tracing::warn!(%error, "failed to read the restore freeze"),
        }
        tokio::select! {
            () = cancellation.cancelled() => return false,
            () = tokio::time::sleep(poll) => {}
        }
    }
}

/// The rescan of one chain since the restore.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainRescan {
    /// EVM chain identifier.
    pub chain_id: u64,
    /// The chain's scanned block when the restore was detected.
    pub restored_block: Option<u64>,
    /// The chain's scanned block now.
    pub scanned_block: Option<u64>,
    /// Block time of the finalized head the scanner last committed through.
    pub scanned_block_time: Option<DateTime<Utc>>,
    /// Issued addresses of the chain whose history the scanner has not read yet, such as
    /// re-issued deposit addresses.
    pub pending_backfills: i64,
    /// Whether a reconciliation block freezes the chain: its scanner is paused, so it is left out
    /// of the rescan, and its crediting stays stopped until the block is lifted.
    pub blocked: bool,
    /// Whether the chain is rescanned: finalized past the moment the restore was detected, and
    /// every issued address backfilled.
    pub complete: bool,
}

/// The rescan since `restore` on every configured chain.
pub async fn rescan_progress(
    pool: &PgPool,
    restore: &Restore,
    routes: &RouteSet,
) -> Result<Vec<ChainRescan>, sqlx::Error> {
    let mut chains = Vec::new();
    for chain_id in routes.chain_ids() {
        let key = i64::try_from(chain_id)
            .map_err(|error| sqlx::Error::Encode(error.to_string().into()))?;
        let (scanned_block, scanned_block_time, pending_backfills): (
            Option<i64>,
            Option<DateTime<Utc>>,
            i64,
        ) = sqlx::query_as(
            r#"
            SELECT (SELECT scanned_block FROM cursors WHERE chain_id = $1),
                   (SELECT scanned_block_time FROM cursors WHERE chain_id = $1),
                   (SELECT count(*) FROM addresses WHERE chain_id = $1 AND NOT backfilled)
            "#,
        )
        .bind(key)
        .fetch_one(pool)
        .await?;
        let blocked = crate::reconciler::chain_is_blocked(pool, chain_id).await?;
        let caught_up = scanned_block_time.is_some_and(|time| time >= restore.detected_at);
        chains.push(ChainRescan {
            chain_id,
            restored_block: restore.restored_cursors.get(&chain_id).copied(),
            scanned_block: scanned_block.and_then(|block| u64::try_from(block).ok()),
            scanned_block_time,
            pending_backfills,
            blocked,
            complete: blocked || (caught_up && pending_backfills == 0),
        });
    }
    Ok(chains)
}

/// Why the service cannot be unfrozen.
#[derive(Debug, thiserror::Error)]
pub enum UnfreezeError {
    /// No restore freeze is active.
    #[error("the service is not frozen")]
    NotFrozen,
    /// A chain is not rescanned yet.
    #[error("the rescan since the restore is incomplete")]
    RescanIncomplete(Vec<ChainRescan>),
    /// PostgreSQL rejected or failed the operation.
    #[error("{0}")]
    Database(#[from] sqlx::Error),
}

/// Lifts the active freeze once every chain is rescanned, recording who and why in the restore
/// and in `audit` in one transaction.
pub async fn unfreeze(
    pool: &PgPool,
    routes: &RouteSet,
    actor: &Actor,
    reason: &str,
) -> Result<Restore, UnfreezeError> {
    let mut transaction = pool.begin().await?;
    let restore = active_in(&mut transaction)
        .await?
        .ok_or(UnfreezeError::NotFrozen)?;
    let chains = rescan_progress(pool, &restore, routes).await?;
    if chains.iter().any(|chain| !chain.complete) {
        return Err(UnfreezeError::RescanIncomplete(chains));
    }
    let unfrozen: Restore = sqlx::query_as::<_, RestoreRow>(sqlx::AssertSqlSafe(format!(
        "UPDATE restores SET unfrozen_at = now(), unfrozen_by = $2, unfreeze_reason = $3 \
         WHERE id = $1 RETURNING {RESTORE_COLUMNS}"
    )))
    .bind(restore.id)
    .bind(actor.to_string())
    .bind(reason)
    .fetch_one(&mut *transaction)
    .await?
    .try_into()?;
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: None,
            actor,
            action: "restore.unfreeze",
            subject: &format!("restore:{}", restore.id),
            reason,
        },
    )
    .await?;
    transaction.commit().await?;
    tracing::warn!(restore_id = %restore.id, "restore freeze lifted by the operator");
    Ok(unfrozen)
}

/// An event as the merchant received it, with its identity checked by the caller.
#[derive(Clone, Debug, PartialEq)]
pub struct DeliveredEvent {
    /// Event id, `event_id(type, deposit)`.
    pub id: Uuid,
    /// The event's account.
    pub account_id: Uuid,
    /// The event's mode.
    pub livemode: bool,
    /// One of [`REDERIVED_EVENT_TYPES`].
    pub event_type: String,
    /// The deposit the event is about.
    pub deposit_id: Uuid,
    /// The event's `created`.
    pub created: DateTime<Utc>,
    /// The event's `actor`.
    pub actor: String,
    /// The event's `data` exactly as delivered.
    pub data: Value,
}

/// What importing a delivered event did.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportOutcome {
    /// The snapshot was stored as the event, with no delivery.
    Imported,
    /// The event was recorded already with the same `data`.
    Matches,
    /// The event was recorded already with other `data`; the stored snapshot is kept.
    Mismatch,
}

impl ImportOutcome {
    /// The API code.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Imported => "imported",
            Self::Matches => "matches",
            Self::Mismatch => "mismatch",
        }
    }
}

/// Stores `event`, delivered to the merchant after the restore point and so lost, as the event it
/// is: a rescan that re-derives its deposit then finds the event recorded and emits nothing, so
/// the merchant never receives it again with another body. An event already recorded keeps its
/// stored snapshot; a different delivered body is a mismatch, logged for the operator.
pub async fn import_delivered_event(
    pool: &PgPool,
    restore: &Restore,
    event: &DeliveredEvent,
    actor: &Actor,
) -> Result<ImportOutcome, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let inserted = sqlx::query(
        r#"
        INSERT INTO events (id, account_id, livemode, type, object_type, object_id, actor, data,
                            created)
        VALUES ($1, $2, $3, $4, 'deposit', $5, $6, $7, $8)
        ON CONFLICT (id) DO NOTHING
        "#,
    )
    .bind(event.id)
    .bind(event.account_id)
    .bind(event.livemode)
    .bind(&event.event_type)
    .bind(event.deposit_id)
    .bind(&event.actor)
    .bind(&event.data)
    .bind(event.created)
    .execute(&mut *transaction)
    .await?
    .rows_affected()
        > 0;
    let outcome = if inserted {
        sqlx::query("INSERT INTO restore_delivered_events (event_id, restore_id) VALUES ($1, $2)")
            .bind(event.id)
            .bind(restore.id)
            .execute(&mut *transaction)
            .await?;
        ImportOutcome::Imported
    } else {
        let stored: Value = sqlx::query_scalar("SELECT data FROM events WHERE id = $1")
            .bind(event.id)
            .fetch_one(&mut *transaction)
            .await?;
        if stored == event.data {
            ImportOutcome::Matches
        } else {
            tracing::error!(
                event_id = %event.id,
                restore_id = %restore.id,
                "a delivered event differs from the recorded one; the recorded snapshot is kept"
            );
            ImportOutcome::Mismatch
        }
    };
    audit::insert(
        &mut *transaction,
        &audit::Entry {
            account_id: Some(event.account_id),
            actor,
            action: "restore.import_event",
            subject: &format!("event:{}", crate::ids::format(crate::ids::EVENT, event.id)),
            reason: &format!("restore {}: {}", restore.id, outcome.code()),
        },
    )
    .await?;
    transaction.commit().await?;
    Ok(outcome)
}

/// An imported event whose deposit the ledger does not yet hold as delivered.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeliveredEventFinding {
    /// Event id.
    pub event_id: Uuid,
    /// Event type.
    pub event_type: String,
    /// The deposit.
    pub deposit_id: Uuid,
    /// `pending` (the rescan has not re-derived or valued the deposit yet) or `mismatch` (the
    /// ledger's token amount or credit differs from the delivered one).
    pub status: &'static str,
    /// Delivered `amount_atomic`.
    pub delivered_amount_atomic: Option<String>,
    /// Delivered `amount` (the credit in minor units).
    pub delivered_amount: Option<String>,
    /// The ledger's `amount_atomic`.
    pub ledger_amount_atomic: Option<String>,
    /// The ledger's credit in minor units.
    pub ledger_amount: Option<String>,
}

#[derive(FromRow)]
struct ImportedRow {
    event_id: Uuid,
    event_type: String,
    deposit_id: Uuid,
    delivered_amount_atomic: Option<String>,
    delivered_amount: Option<String>,
    ledger_amount_atomic: Option<String>,
    ledger_amount: Option<String>,
}

/// Imported events of `restore` compared with the ledger: the count, and each one whose deposit
/// is not re-derived yet or whose amounts differ from what the merchant received. The delivered
/// snapshot stays the event; a mismatch is the operator's to settle with the merchant.
pub async fn delivered_event_findings(
    pool: &PgPool,
    restore: &Restore,
) -> Result<(i64, Vec<DeliveredEventFinding>), sqlx::Error> {
    let rows = sqlx::query_as::<_, ImportedRow>(
        r#"
        SELECT event.id AS event_id, event.type AS event_type, event.object_id AS deposit_id,
               event.data #>> '{object,amount_atomic}' AS delivered_amount_atomic,
               event.data #>> '{object,amount}' AS delivered_amount,
               deposit.amount_atomic::text AS ledger_amount_atomic,
               deposit.credit_minor::text AS ledger_amount
        FROM restore_delivered_events AS imported
        JOIN events AS event ON event.id = imported.event_id
        LEFT JOIN deposits AS deposit ON deposit.id = event.object_id
        WHERE imported.restore_id = $1
        ORDER BY event.created, event.id
        "#,
    )
    .bind(restore.id)
    .fetch_all(pool)
    .await?;
    let imported = i64::try_from(rows.len()).unwrap_or(i64::MAX);
    let findings = rows
        .into_iter()
        .filter_map(|row| {
            let status = match (
                &row.ledger_amount_atomic,
                &row.delivered_amount,
                &row.ledger_amount,
            ) {
                (None, _, _) | (Some(_), Some(_), None) => "pending",
                (Some(ledger), _, _) if Some(ledger) != row.delivered_amount_atomic.as_ref() => {
                    "mismatch"
                }
                (Some(_), Some(delivered), Some(ledger)) if delivered != ledger => "mismatch",
                _ => return None,
            };
            Some(DeliveredEventFinding {
                event_id: row.event_id,
                event_type: row.event_type,
                deposit_id: row.deposit_id,
                status,
                delivered_amount_atomic: row.delivered_amount_atomic,
                delivered_amount: row.delivered_amount,
                ledger_amount_atomic: row.ledger_amount_atomic,
                ledger_amount: row.ledger_amount,
            })
        })
        .collect();
    Ok((imported, findings))
}
